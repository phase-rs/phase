//! Issue #8750 — The Great Work's chapter III, driven through the real Saga
//! pipeline.
//!
//! Chapter III prints "Until end of turn, you may cast instant and sorcery
//! spells from any graveyard. If a spell cast this way would be put into a
//! graveyard, exile it instead. Exile this Saga, then return it to the
//! battlefield (front face up)."
//!
//! Before this fix the first sentence lowered to a `CastFromZone` that names no
//! card, so it granted nothing: no graveyard card was castable afterwards. And
//! the rider branch that consumed it dropped the two-link "Exile this Saga, then
//! return it" tail, so CR 714.4 sacrificed the Saga and Urabrask went to the
//! graveyard. The first two sentences now lower to one graveyard cast
//! permission over every player's graveyard (CR 404.1 + CR 611.2c), and the
//! self-flicker runs as an ordinary chain behind it.
//!
//! CR 400.7j: "If an effect causes an object to move to a public zone, other
//! parts of that effect can find that object." — that is what lets "return it"
//! find the card "Exile this Saga" just moved. CR 712.14: a double-faced card
//! put onto the battlefield from a zone other than the stack enters front face
//! up by default.
//!
//! The card is built from its printed text rather than loaded by name (CI has
//! no card database): the front is a plain creature named for Urabrask, the
//! back face is The Great Work's verbatim Oracle text
//! (`client/public/card-data.json`), and the permanent is turned back face up
//! through the production `transform_permanent`. The lore counter that fires
//! chapter III comes from the CR 714.3c turn-based action, not from the test.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::printed_cards::snapshot_object_face;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::{transform, zones};
use engine::types::ability::{CardPlayMode, TargetFilter, TypeFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, CastingVariant, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{FlashbackCost, Keyword};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::statics::{CastFrequency, GraveyardPermissionPool, StaticMode};
use engine::types::zones::Zone;
use engine::types::{CardId, StaticDefinition};

/// The Great Work, verbatim.
const THE_GREAT_WORK: &str =
    "(As this Saga enters and after your draw step, add a lore counter.)\n\
I — This Saga deals 3 damage to target opponent and each creature they control.\n\
II — Create three Treasure tokens.\n\
III — Until end of turn, you may cast instant and sorcery spells from any graveyard. If a \
spell cast this way would be put into a graveyard, exile it instead. Exile this Saga, then \
return it to the battlefield (front face up).";

/// Park the game at the end of P0's turn, so the next two `advance_to_phase`
/// calls reach P1's, then P0's, precombat main (CR 714.3c).
fn park_for_next_p0_precombat_main(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 1;
    state.active_player = P0;
    state.phase = Phase::End;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

/// Resolve everything on the stack, answering trigger-order prompts.
fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..64 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            let _ = runner.act(GameAction::PassPriority);
            let _ = runner.act(GameAction::PassPriority);
        } else {
            break;
        }
    }
}

/// The objects a test reads after chapter III has resolved.
struct ChapterThree {
    runner: GameRunner,
    urabrask: ObjectId,
    /// An instant in the OPPONENT's graveyard before chapter III resolves.
    opponents_instant: ObjectId,
    /// An instant in P0's hand, cast after chapter III resolved.
    later_instant: ObjectId,
}

/// Urabrask // The Great Work, back face up with two lore counters, driven
/// through P0's next precombat main so chapter III triggers and resolves.
fn resolve_chapter_three() -> ChapterThree {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest"]);

    let urabrask = scenario.add_creature(P0, "Urabrask", 4, 4).id();
    // Supplies the back face; removed from the battlefield before play starts.
    let donor = scenario
        .add_creature(P0, "The Great Work", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(THE_GREAT_WORK)
        .id();
    let opponents_instant = scenario
        .add_spell_to_graveyard(P1, "Opponent's Instant", true)
        .id();
    let later_instant = scenario
        .add_spell_to_hand_from_oracle(P0, "Later Instant", true, "Draw a card.")
        .id();
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        let mut back_face = snapshot_object_face(&state.objects[&donor]);
        back_face.card_types.core_types = vec![CoreType::Enchantment];
        zones::remove_from_zone(state, donor, Zone::Battlefield, P0);
        state.objects.remove(&donor);
        state.objects.get_mut(&urabrask).unwrap().back_face = Some(back_face);
        let mut events = Vec::new();
        transform::transform_permanent(state, urabrask, &mut events)
            .expect("Urabrask transforms into The Great Work");
        state
            .objects
            .get_mut(&urabrask)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 2);
    }
    {
        let saga = &runner.state().objects[&urabrask];
        assert!(
            saga.transformed && saga.name == "The Great Work",
            "precondition: the permanent is The Great Work, back face up, got {} (transformed = {})",
            saga.name,
            saga.transformed
        );
    }

    // CR 714.3c: the next precombat main adds the third lore counter, which
    // triggers chapter III (CR 714.2b).
    park_for_next_p0_precombat_main(&mut runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    drain_stack(&mut runner);

    // Reach guard: the Saga left the battlefield this turn. Either chapter
    // III's tail exiled it, or — tail dropped — CR 714.4 sacrificed it once
    // chapter III had left the stack. Both need the third lore counter.
    assert!(
        runner.state().zone_changes_this_turn.iter().any(|record| {
            record.object_id == urabrask && record.from_zone == Some(Zone::Battlefield)
        }),
        "reach guard: the Saga must have reached its third lore counter and left the \
         battlefield"
    );

    ChapterThree {
        runner,
        urabrask,
        opponents_instant,
        later_instant,
    }
}

#[test]
fn great_work_chapter_three_8750() {
    let ChapterThree {
        runner, urabrask, ..
    } = resolve_chapter_three();

    let returned = &runner.state().objects[&urabrask];
    assert_eq!(
        returned.zone,
        Zone::Battlefield,
        "\"then return it to the battlefield\" must bring the card back (CR 400.7j), not \
         leave it in exile or let CR 714.4 sacrifice the Saga into the graveyard"
    );
    assert!(
        !returned.transformed && returned.name == "Urabrask",
        "the card returns front face up (CR 712.14), got {} (transformed = {})",
        returned.name,
        returned.transformed
    );
    assert!(
        returned.card_types.core_types.contains(&CoreType::Creature),
        "front face up means it is the creature Urabrask again"
    );
}

/// CR 404.1 + CR 601.3 + CR 611.2c + CR 614.1a: "you may cast instant and
/// sorcery spells from any graveyard" reaches the opponent's graveyard, covers a
/// card that reaches a graveyard after the chapter resolved, exiles a spell cast
/// this way instead of putting it into a graveyard, and ends with the turn.
#[test]
fn great_work_chapter_three_casts_from_any_graveyard_until_end_of_turn() {
    let ChapterThree {
        mut runner,
        opponents_instant,
        later_instant,
        ..
    } = resolve_chapter_three();

    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&opponents_instant),
        "CR 404.1: \"any graveyard\" includes the opponent's"
    );
    runner.cast(opponents_instant).resolve();
    assert_eq!(
        runner.state().objects[&opponents_instant].zone,
        Zone::Exile,
        "CR 614.1a: a spell cast this way is exiled instead of going to its owner's graveyard"
    );

    // CR 611.2c: the permission changes the rules, not any object's
    // characteristics, so it also covers a card that reaches a graveyard later.
    runner.cast(later_instant).resolve();
    assert_eq!(
        runner.state().objects[&later_instant].zone,
        Zone::Graveyard,
        "precondition: the hand-cast instant resolved into P0's graveyard"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&later_instant),
        "CR 611.2c: a card put into a graveyard after the chapter resolved is castable too"
    );

    // CR 611.2a + CR 514.2: "until end of turn" — gone on the next turn, when
    // the instant could otherwise be cast at instant speed.
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "precondition: the turn passed"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&later_instant),
        "CR 611.2a: the permission ended with the turn"
    );
}

/// CR 109.5 + CR 702.34a: flashback means "you may cast this card from your
/// graveyard", and "you" is the player attempting the cast. Under chapter III an
/// opponent's flashback instant is castable only through the permission, so no
/// Flashback option is offered for it; the same keyword on the caster's own
/// graveyard card still is. The YES/NO pair at one board.
#[test]
fn great_work_offers_no_flashback_for_an_opponents_card() {
    fn give_flashback(runner: &mut GameRunner, object_id: ObjectId) {
        let flashback = Keyword::Flashback(FlashbackCost::Mana(ManaCost::zero()));
        let obj = runner.state_mut().objects.get_mut(&object_id).unwrap();
        obj.base_keywords.push(flashback.clone());
        obj.keywords.push(flashback);
    }
    fn offers_flashback(runner: &GameRunner) -> bool {
        matches!(
            &runner.state().waiting_for,
            WaitingFor::CastingVariantChoice { options, .. }
                if options.iter().any(|option| option.variant == CastingVariant::Flashback)
        )
    }
    fn announce(runner: &mut GameRunner, object_id: ObjectId) {
        let card_id = runner.state().objects[&object_id].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("the cast is announced");
    }

    let ChapterThree {
        mut runner,
        opponents_instant,
        later_instant,
        ..
    } = resolve_chapter_three();

    give_flashback(&mut runner, opponents_instant);
    announce(&mut runner, opponents_instant);
    assert!(
        !offers_flashback(&runner),
        "CR 109.5: the opponent's flashback is not the caster's to use, got {:?}",
        runner.state().waiting_for
    );
    let variant = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell {
                casting_variant, ..
            } if entry.source_id == opponents_instant => Some(*casting_variant),
            _ => None,
        })
        .expect("the opponent's instant is on the stack");
    assert_ne!(
        variant,
        CastingVariant::Flashback,
        "CR 109.5: a cast naming no method does not fall back to the opponent's flashback"
    );
    drain_stack(&mut runner);
    assert_eq!(
        runner.state().objects[&opponents_instant].zone,
        Zone::Exile,
        "the permission cast the opponent's card and exiled it"
    );

    runner.cast(later_instant).resolve();
    assert_eq!(
        runner.state().objects[&later_instant].zone,
        Zone::Graveyard,
        "precondition: the hand-cast instant resolved into P0's graveyard"
    );
    give_flashback(&mut runner, later_instant);
    announce(&mut runner, later_instant);
    assert!(
        offers_flashback(&runner),
        "CR 702.34a: the caster's own graveyard card keeps its flashback option, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 404.1: the YES/NO pair at one input for the PARSED grant. "From your
/// graveyard" reaches the caster's own graveyard and not the opponent's, while
/// the same opponent's card is castable under "from any graveyard" above.
///
/// Said plainly: the "not the opponent's" half is held twice here — the parser
/// also puts `ControllerRef::You` on the card filter — so it stays green with the
/// pool check removed. The pool alone is pinned by
/// `a_printed_own_graveyard_permission_refuses_the_opponents_card_at_cast`.
#[test]
fn a_your_graveyard_grant_does_not_reach_the_opponents_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own_instant = scenario
        .add_spell_to_graveyard(P0, "Own Instant", true)
        .id();
    let opponents_instant = scenario
        .add_spell_to_graveyard(P1, "Opponent's Instant", true)
        .id();
    let grant = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Grant",
            false,
            "Until end of turn, you may cast instant and sorcery spells from your graveyard.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(grant).resolve();

    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&own_instant),
        "the caster's own graveyard card is castable"
    );
    assert!(
        !castable.contains(&opponents_instant),
        "CR 404.1: \"your graveyard\" is not the opponent's"
    );
}

/// CR 109.5 + CR 404.1: a printed "from your graveyard" permission whose card
/// filter names no controller (the Lurrus / Muldrotha / Karador shape) is kept to
/// its controller's own graveyard by the permission's pool alone. The YES/NO pair
/// at one input, through the real cast: the own card casts, the opponent's card
/// is refused.
#[test]
fn a_printed_own_graveyard_permission_refuses_the_opponents_card_at_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own_instant = scenario
        .add_spell_to_graveyard(P0, "Own Instant", true)
        .id();
    let opponents_instant = scenario
        .add_spell_to_graveyard(P1, "Opponent's Instant", true)
        .id();
    let mut runner = scenario.build();

    let source = zones::create_object(
        runner.state_mut(),
        CardId(8750),
        P0,
        "Permission Source".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("permission source")
        .static_definitions
        .push(
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                graveyard_destination_replacement: None,
                extra_cost: None,
                enters_with_counter: None,
                required_cast_keyword: None,
                pool: GraveyardPermissionPool::OwnGraveyard,
            })
            // No controller on the filter: the pool is the only owner check.
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant))),
        );

    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&own_instant),
        "the permission admits its controller's own graveyard card"
    );
    assert!(
        !castable.contains(&opponents_instant),
        "CR 404.1: \"your graveyard\" is not the opponent's"
    );
    assert!(
        runner.cast(opponents_instant).try_resolve().is_err(),
        "the real cast of the opponent's card must be refused"
    );
    assert_eq!(
        runner.state().objects[&opponents_instant].zone,
        Zone::Graveyard,
        "the refused card stays where it was"
    );
    assert!(
        runner.cast(own_instant).try_resolve().is_ok(),
        "the same permission casts its controller's own card"
    );
}
