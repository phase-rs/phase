//! Issue #8762: `Effect::Counter`'s rider branch in `resolve_chain_body`
//! consumed the CR 614.1a graveyard-exile rider and returned, discarding every
//! `SequentialSibling` link behind it — where the `CastFromZone` branch of the
//! same function already runs such a tail (#6945). Spelljack's third sentence ("You
//! may play it without paying its mana cost for as long as it remains exiled.")
//! and No Escape's "Scry 1." therefore never ran.
//!
//! Three parts, each with its own counter-probe:
//! 1. the rider branch now runs the rider's direct sequential tail for the
//!    families `counter_tail_family_has_runtime_evidence` admits;
//! 2. `affected_objects_with_causes` stamps the countered card `Exiled` when the
//!    counter's exile rider applied to it — without that stamp the tail's
//!    `TrackedSetFiltered { caused_by: Exiled }` anaphor matched nothing, so the
//!    permission grant resolved against an empty set even when it ran;
//! 3. the rider's printed condition decides WHETHER it applies to the concrete
//!    countered spell — `counter::resolve` asks
//!    `cast_from_zone::graveyard_exile_rider_applies_to` once, when it chooses
//!    the destination, and records the answer in `exile_rider_countered_ids`;
//!    the provenance stamp reads that record (an Adventure spell changes face
//!    between the two, CR 715.4). Thranduil's Decree ("If a PERMANENT spell is
//!    countered this way") exiled a countered instant on `main`; it now goes
//!    to its owner's graveyard (CR 701.6a), unstamped and without the
//!    permission.
//!
//! Corpus (`client/public/card-data.json`): 20 counter heads carry the exile
//! rider, 6 of them a tail — Spelljack, Thranduil's Decree, Kheru Spellsnatcher
//! (`CastFromZone`, one family in two modes), No Escape (`Scry`), Delay (`GenericEffect`),
//! Devious Cover-Up (`ChangeZone` → `Shuffle`). The first four change here; the
//! last two are out of scope with measured reasons, pinned below as unchanged.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CastingPermission, Effect, SubAbilityLink, ThisWayCause,
};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Oracle texts verbatim from `client/public/card-data.json`.
const SPELLJACK: &str = "Counter target spell. If that spell is countered this way, exile it \
                         instead of putting it into its owner's graveyard. You may play it \
                         without paying its mana cost for as long as it remains exiled. (If it \
                         has X in its mana cost, X is 0.)";
const THRANDUILS_DECREE: &str = "Counter target spell. If a permanent spell is countered this \
                                 way, exile it instead of putting it into its owner's \
                                 graveyard. You may cast that card without paying its mana \
                                 cost for as long as it remains exiled.";
const NO_ESCAPE: &str = "Counter target creature or planeswalker spell. If that spell is \
                         countered this way, exile it instead of putting it into its owner's \
                         graveyard.\nScry 1.";
const DELAY: &str = "Counter target spell. If the spell is countered this way, exile it with \
                     three time counters on it instead of putting it into its owner's \
                     graveyard. If it doesn't have suspend, it gains suspend. (At the \
                     beginning of its owner's upkeep, they remove a time counter. When the \
                     last is removed, they may play it without paying its mana cost. If it's \
                     a creature, it has haste.)";
const DEVIOUS_COVER_UP: &str = "Counter target spell. If that spell is countered this way, \
                                exile it instead of putting it into its owner's graveyard. You \
                                may shuffle up to four target cards from your graveyard into \
                                your library.";

/// An opponent spell on the stack, mirroring `counter_spell_zone_redirect.rs`,
/// cast as `variant`. For `CastingVariant::Adventure` the object is
/// the Adventure half (type `core`) with the creature face stored as its back
/// face — the shape `counter::resolve` restores when the spell leaves the
/// stack (CR 715.4).
fn put_spell_on_stack_as(
    runner: &mut GameRunner,
    controller: PlayerId,
    core: CoreType,
    variant: CastingVariant,
) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(701),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        if variant == CastingVariant::Adventure {
            let mut creature_face = engine::game::printed_cards::snapshot_object_face(obj);
            creature_face.name = "Bonecrusher Giant".to_string();
            creature_face.card_types.core_types = vec![CoreType::Creature];
            creature_face.layout_kind = Some(LayoutKind::Adventure);
            obj.back_face = Some(creature_face);
            obj.name = "Stomp".to_string();
        }
        obj.card_types.core_types = vec![core];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(701),
            ability: None,
            casting_variant: variant,
            actual_mana_spent: 0,
        },
    });
    spell
}

/// P0 casts `oracle` at an opponent spell of type `core` and resolves it.
/// Returns the runner, the countered spell and the resolution's events.
fn counter_with(
    name: &str,
    oracle: &str,
    core: CoreType,
    setup: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, Vec<GameEvent>) {
    counter_with_variant(name, oracle, core, CastingVariant::Normal, setup)
}

/// As `counter_with`, with the opponent spell cast as `variant`.
fn counter_with_variant(
    name: &str,
    oracle: &str,
    core: CoreType,
    variant: CastingVariant,
    setup: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut cs = scenario.add_spell_to_hand_from_oracle(P0, name, true, oracle);
    cs.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Blue],
    });
    let counter = cs.id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    setup(&mut scenario);
    let mut runner = scenario.build();
    let opponent_spell = put_spell_on_stack_as(&mut runner, P1, core, variant);

    let outcome = runner
        .cast(counter)
        .target_objects(&[opponent_spell])
        .try_resolve()
        .expect("the counter must cast and resolve");
    let events = outcome.events().to_vec();
    (runner, opponent_spell, events)
}

/// Reach guard shared by every test: the counter and its exile rider really
/// ran, so a failure below is about the tail and nothing upstream of it.
fn assert_countered_into_exile(runner: &GameRunner, countered: ObjectId) {
    assert!(
        runner.state().stack.is_empty(),
        "the spell must be countered (off the stack)"
    );
    assert_eq!(
        runner.state().objects[&countered].zone,
        Zone::Exile,
        "the rider must exile the countered spell instead of the graveyard"
    );
}

/// The permission is USABLE, not merely recorded: P0 (who holds priority after
/// their spell resolved) has a `CastSpell` action for the exiled card.
fn can_cast(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

/// The provenance the counter published for `id`: the `ThisWayCause` stamped
/// on it in any tracked set (`chain_tracked_set_id` is cleared once the chain
/// ends, so every set is searched). `None` when the card was never stamped.
fn published_cause(runner: &GameRunner, id: ObjectId) -> Option<ThisWayCause> {
    runner
        .state()
        .tracked_set_member_causes
        .values()
        .find_map(|causes| causes.get(&id).copied())
}

/// CR 608.2c + CR 614.1a: Spelljack's third sentence is an instruction of the
/// same resolution and runs after the rider. On `main` the countered card was
/// exiled with NO casting permission.
///
/// Two carriers are the established shape of a `mode: Play` free grant, not a
/// double grant: `cast_from_zone::resolve` records the cast half
/// (`ExileWithAltCost { zero }`) and, because CR 305.1 has lands PLAYED rather
/// than cast, a `PlayFromExile` land companion alongside it — the same pair the
/// `lasting_play_from_exile_permission` tests describe. The `mode: Cast` sibling
/// below records one.
#[test]
fn spelljack_grants_the_play_permission_after_exiling_the_countered_spell() {
    let (runner, countered, _) = counter_with("Spelljack", SPELLJACK, CoreType::Instant, |_| {});
    assert_countered_into_exile(&runner, countered);

    assert!(
        can_cast(&runner, countered),
        "\"You may play it without paying its mana cost for as long as it remains exiled\" \
         must make the exiled card castable by Spelljack's controller (issue #8762)"
    );
    let permissions = &runner.state().objects[&countered].casting_permissions;
    assert_eq!(
        permissions.len(),
        2,
        "a `mode: Play` free grant records the cast half and its CR 305.1 land companion, \
         got {permissions:?}"
    );
    assert!(
        permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::zero())),
        "the cast half must be a zero-cost `ExileWithAltCost`, got {permissions:?}"
    );
}

/// Thranduil's Decree is the card of the user-facing report (#7132) and the
/// `mode: Cast` member of the class: "You may CAST that card", so exactly one
/// carrier and no land companion. Driven with a permanent spell, the case its
/// rider names.
///
/// The rider's "if a PERMANENT spell" condition is `ZoneChangedThisWay {
/// Permanent }` on the rider; the sibling test below drives the instant case.
#[test]
fn thranduils_decree_grants_the_cast_permission_after_exiling_a_permanent_spell() {
    let (runner, countered, _) = counter_with(
        "Thranduil's Decree",
        THRANDUILS_DECREE,
        CoreType::Creature,
        |_| {},
    );
    assert_countered_into_exile(&runner, countered);
    assert_eq!(
        published_cause(&runner, countered),
        Some(ThisWayCause::Exiled),
        "a countered permanent spell is published as \"exiled this way\""
    );

    assert!(
        can_cast(&runner, countered),
        "\"You may cast that card without paying its mana cost for as long as it remains \
         exiled\" must make the exiled card castable (issues #8762, #7132)"
    );
    let permissions = &runner.state().objects[&countered].casting_permissions;
    assert_eq!(
        permissions.len(),
        1,
        "a `mode: Cast` grant records the cast half only, got {permissions:?}"
    );
}

/// The rider's printed condition decides whether it applies. Thranduil's Decree
/// names "a PERMANENT spell": a countered INSTANT is not exiled — it goes to
/// its owner's graveyard (CR 701.6a) — is not published as "exiled this way",
/// and receives no permission. On `main` (and on this PR's first head) the
/// instant was exiled and stamped `Exiled`, because `counter::resolve` and the
/// stamp read the rider's presence alone; only the tail was withheld.
///
/// Sibling of the creature test above, on the same setup: the two together are
/// the ONLY difference the condition makes, so a destination gate that ignores
/// the condition turns this test red on the zone and the provenance, and one
/// that never exiles turns the sibling red.
#[test]
fn thranduils_decree_sends_a_countered_instant_to_the_graveyard_without_provenance_or_permission() {
    let (runner, countered, _) = counter_with(
        "Thranduil's Decree",
        THRANDUILS_DECREE,
        CoreType::Instant,
        |_| {},
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: the counter must resolve and take the spell off the stack"
    );

    assert_eq!(
        runner.state().objects[&countered].zone,
        Zone::Graveyard,
        "\"If a permanent spell is countered this way\" does not apply to an instant, so \
         CR 701.6a puts it into its owner's graveyard (issue #8762)"
    );
    assert_eq!(
        published_cause(&runner, countered),
        None,
        "a countered instant was not exiled and must not be published as \"exiled this way\""
    );
    assert!(
        !can_cast(&runner, countered),
        "\"you may cast that card\" must not follow for a card the rider did not exile"
    );
    assert!(
        runner.state().objects[&countered]
            .casting_permissions
            .is_empty(),
        "no permission may be recorded on a card the rider's condition excluded, got {:?}",
        runner.state().objects[&countered].casting_permissions
    );
}

/// The rider is applied to the spell AS CAST, and the answer is carried to the
/// stamp rather than re-derived: an Adventure spell (Stomp, an instant) has its
/// creature face (Bonecrusher Giant) restored by `counter::resolve` right after
/// the destination is chosen (CR 715.4), so a stamp that re-asked "is it a
/// permanent spell?" afterwards would say yes about a card in the graveyard —
/// publishing it as "exiled this way" and handing the free cast to a graveyard
/// card. Review-round finding on this PR; the ledger
/// `exile_rider_countered_ids` exists for this case.
#[test]
fn thranduils_decree_on_an_adventure_instant_does_not_stamp_the_restored_creature_face() {
    let (runner, countered, _) = counter_with_variant(
        "Thranduil's Decree",
        THRANDUILS_DECREE,
        CoreType::Instant,
        CastingVariant::Adventure,
        |_| {},
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: the counter must resolve and take the spell off the stack"
    );
    assert_eq!(
        runner.state().objects[&countered].card_types.core_types,
        vec![CoreType::Creature],
        "reach guard: the creature face is restored once the Adventure spell left the stack"
    );

    assert_eq!(
        runner.state().objects[&countered].zone,
        Zone::Graveyard,
        "the Adventure half is an instant when countered, so it goes to the graveyard"
    );
    assert_eq!(
        published_cause(&runner, countered),
        None,
        "the restored creature face must not turn a graveyard card into \"exiled this way\""
    );
    assert!(
        !can_cast(&runner, countered),
        "no free cast for a card the rider did not exile"
    );
    assert!(
        runner.state().objects[&countered]
            .casting_permissions
            .is_empty(),
        "got {:?}",
        runner.state().objects[&countered].casting_permissions
    );
}

/// Positive partner on the same Adventure setup: Spelljack's rider names "that
/// spell" (`Typed[Card]`), so the Adventure half IS exiled, stamped, and
/// castable — the face restore does not lose a stamp that was earned.
#[test]
fn spelljack_on_an_adventure_instant_exiles_stamps_and_grants_the_play_permission() {
    let (runner, countered, _) = counter_with_variant(
        "Spelljack",
        SPELLJACK,
        CoreType::Instant,
        CastingVariant::Adventure,
        |_| {},
    );
    assert_countered_into_exile(&runner, countered);
    assert_eq!(
        runner.state().objects[&countered].card_types.core_types,
        vec![CoreType::Creature],
        "reach guard: the creature face is restored once the Adventure spell left the stack"
    );
    assert_eq!(
        published_cause(&runner, countered),
        Some(ThisWayCause::Exiled),
        "the exiled Adventure card is published as \"exiled this way\""
    );
    assert!(
        can_cast(&runner, countered),
        "\"You may play it without paying its mana cost\" applies to the exiled card"
    );
}

/// The `Scry` family: No Escape's second line runs after the rider. The
/// scenario runner answers the `ScryChoice` by keeping the card on top (a
/// harness convention — CR 701.22a leaves the split to the player), so the
/// evidence is the scry EVENT, not a pending choice: a
/// `PlayerPerformedAction { Scry, look_count: 1 }` for P0 in the resolution's
/// events. On `main` no such event is emitted.
#[test]
fn no_escape_scries_after_exiling_the_countered_spell() {
    let (runner, countered, events) =
        counter_with("No Escape", NO_ESCAPE, CoreType::Creature, |scenario| {
            for _ in 0..3 {
                scenario.add_card_to_library_top(P0, "Island");
            }
        });
    assert_countered_into_exile(&runner, countered);

    let scry = events.iter().find(|event| {
        matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::Scry,
                ..
            } if *player_id == P0
        )
    });
    let Some(GameEvent::PlayerPerformedAction { look_count, .. }) = scry else {
        panic!("\"Scry 1.\" must run after the exile rider (issue #8762); events: {events:#?}");
    };
    assert_eq!(*look_count, Some(1), "Scry 1 looks at exactly one card");
}

/// The rider's condition gates only a tail that reads the countered card. No
/// Escape's "Scry 1." is printed unconditionally: against a CR 101.2
/// uncounterable spell the counter moves nothing and the rider's "if that spell
/// is countered this way" is false — the scry must still happen (CR 608.2c, the
/// instructions are followed in order; only the rider's sentence carries the
/// "if"). Under a gate applied to every tail this test is red on the scry.
///
/// The uncounterable spell is a creature spell under Rhythm of the Wild (text
/// verbatim from `client/public/card-data.json`), so the counter itself
/// resolves and is refused at CR 101.2 — the reach guard is the spell staying on
/// the stack.
#[test]
fn no_escape_scries_even_when_the_counter_is_refused() {
    const RHYTHM_OF_THE_WILD: &str = "Creature spells you control can't be countered.\nNontoken creatures you control have riot. (They enter with your choice of a +1/+1 counter or haste.)";
    let (runner, uncounterable, events) =
        counter_with("No Escape", NO_ESCAPE, CoreType::Creature, |scenario| {
            scenario.add_enchantment_from_oracle(P1, "Rhythm of the Wild", RHYTHM_OF_THE_WILD);
            for _ in 0..3 {
                scenario.add_card_to_library_top(P0, "Island");
            }
        });
    assert_eq!(
        runner.state().objects[&uncounterable].zone,
        Zone::Stack,
        "reach guard: the creature spell must survive the counter (CR 101.2), or this test \
         is the ordinary scry test again"
    );

    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::Scry,
                ..
            } if *player_id == P0
        )),
        "\"Scry 1.\" is unconditional and must run even though the rider's condition is \
         false (issue #8762); events: {events:#?}"
    );
}

/// Walk a parsed chain and return the effect two links under a `Counter`
/// (head → rider → tail), if the tail is a `SequentialSibling`.
fn rider_tail(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
    fn walk(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
        if matches!(*def.effect, Effect::Counter { .. }) {
            let rider = def.sub_ability.as_deref()?;
            let tail = rider.sub_ability.as_deref()?;
            return (tail.sub_link == SubAbilityLink::SequentialSibling).then_some(tail);
        }
        def.sub_ability.as_deref().and_then(walk)
    }
    walk(def)
}

/// Characterisation, not a pin of this change: Delay's tail is a
/// `GenericEffect` and Devious Cover-Up's a two-link `ChangeZone` → `Shuffle`,
/// both outside the allowlist. Under a probe that ran them anyway, with the
/// parent context supplied, the exiled card had no suspend after layer
/// evaluation, and Devious Cover-Up's own target slots were never announced so
/// its shuffle moved nothing — so this test is green with the change, without
/// it, and with the allowlist opened. What it does pin is the PARSE: each chain carries the tail
/// as a `SequentialSibling` under the rider, so their exclusion is a decision
/// about a tail that exists. The allowlist itself is pinned by
/// `counter_tail_family_has_runtime_evidence`'s unit test.
#[test]
fn delay_and_devious_cover_up_tails_are_parsed_but_inert() {
    // Delay: the parse carries the tail; the exiled card gains nothing.
    let parsed = parse_oracle_text(DELAY, "Delay", &[], &["Instant".to_string()], &[]);
    let tail = parsed
        .abilities
        .iter()
        .find_map(rider_tail)
        .expect("reach guard: Delay's chain must carry a tail under the rider");
    assert!(
        matches!(*tail.effect, Effect::GenericEffect { .. }),
        "Delay's tail is the suspend grant, got {:?}",
        tail.effect
    );
    let (mut runner, countered, _) = counter_with("Delay", DELAY, CoreType::Creature, |_| {});
    assert_countered_into_exile(&runner, countered);
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert!(
        !engine::game::keywords::has_keyword_kind(
            &runner.state().objects[&countered],
            engine::types::keywords::KeywordKind::Suspend
        ),
        "Delay's suspend grant is inert today (and outside the allowlist)"
    );

    // Devious Cover-Up: the parse carries a two-link tail; the graveyard is untouched.
    let parsed = parse_oracle_text(
        DEVIOUS_COVER_UP,
        "Devious Cover-Up",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let tail = parsed
        .abilities
        .iter()
        .find_map(rider_tail)
        .expect("reach guard: Devious Cover-Up's chain must carry a tail under the rider");
    assert!(
        matches!(*tail.effect, Effect::ChangeZone { .. }) && tail.sub_ability.is_some(),
        "Devious Cover-Up's tail is a two-link shuffle, got {:?}",
        tail.effect
    );
    let mut graveyard = Vec::new();
    let (runner, countered, _) = counter_with(
        "Devious Cover-Up",
        DEVIOUS_COVER_UP,
        CoreType::Creature,
        |scenario| {
            for _ in 0..2 {
                graveyard.push(
                    scenario
                        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
                        .id(),
                );
            }
        },
    );
    assert_countered_into_exile(&runner, countered);
    for card in graveyard {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Graveyard,
            "Devious Cover-Up's shuffle moves nothing today — this cast announces only the \
             counter's target, and the tail is outside the allowlist"
        );
    }
}
