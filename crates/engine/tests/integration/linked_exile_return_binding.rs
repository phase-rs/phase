//! CR 607.2a + CR 406.6: "the exiled card" / "the exiled cards" in a return
//! whose own resolution chain has prior tracked-set publishers, none of them an
//! exile instruction, refers only to cards put into exile by the linked exile
//! instruction — never to the chain's own sacrificed or library-put members.
//! The parser binds the bare chain sentinel, deciding it once on the finished tree, by where the
//! holding ability is written (CR 607.1). A return printed on the source binds the source's
//! linked exile (`ExiledBySource`, CR 607.2a) and moves set-wide out of exile. A return in an
//! ability an effect creates (Timothar's Bat trigger, CR 607.1d) binds the `Exiled`-caused
//! subset (`TrackedSetFiltered { id: 0, filter: Any, caused_by: Some(Exiled) }`, D8).
//!
//! Runtime rows (R1, DR1-DR4, R9 as DR3, S1) drive the real cast pipeline through `GameScenario`
//! with cards built from live-parsed Oracle text (never the committed fixture),
//! so a START run measures START's parser. START is the parser before this change,
//! which binds the bare sentinel `TrackedSet(0)` everywhere; D8 is the `Exiled`-caused
//! binding below. Shape rows (R2–R6, R8, R10–R118) parse with
//! `parse_oracle_text`, the production parse entry.
//!
//! Oracle text, byte-verbatim from MTGJSON `AtomicCards.json` (5.3.0+20261004).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, ContinuousModification, ControllerRef, Effect,
    EffectOutcomeSignal, TargetFilter, ThisWayCause,
};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::phase::Phase;
use engine::types::zones::{EtbTapState, Zone};
use serde_json::Value;

const DAY_OF_THE_DRAGONS: &str = "When this enchantment enters, exile all creatures you control. Then create that many 5/5 red Dragon creature tokens with flying.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control.";
const TIMOTHAR: &str = "Ward\u{2014}Discard a card.\nWhenever another nontoken Vampire you control dies, you may pay {1} and exile it. If you do, create a 1/1 black Bat creature token with flying. It gains \"When this token deals combat damage to a player, sacrifice it and return the exiled card to the battlefield tapped.\"";
const THE_SPOT: &str = "When The Spot enters, exile up to one target nonland permanent and up to one target nonland permanent card from a graveyard.\nWhen The Spot dies, put him on the bottom of his owner's library. If you do, return the exiled cards to their owners' hands.";
const OBLIVION_RING: &str = "When this enchantment enters, exile another target nonland permanent.\nWhen this enchantment leaves the battlefield, return the exiled card to the battlefield under its owner's control.";
const TIDEHOLLOW_SCULLER: &str = "When this creature enters, target opponent reveals their hand and you choose a nonland card from it. Exile that card.\nWhen this creature leaves the battlefield, return the exiled card to its owner's hand.";
/// Synthetic face: a delayed "the exiled cards" return created by a chain whose
/// earlier clause is a non-exile publisher (the sacrifice), with no exile
/// producer anywhere in the creating chain.
const SYNTHETIC_DELAYED_RETURN_AFTER_SACRIFICE: &str = "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Return the exiled cards to the battlefield under your control at the beginning of the next end step.";
const SUDDEN_DISAPPEARANCE: &str = "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step.";
const INDOMITABLE_CREATIVITY: &str = "Destroy X target artifacts and/or creatures. For each permanent destroyed this way, its controller reveals cards from the top of their library until an artifact or creature card is revealed and exiles that card. Those players put the exiled cards onto the battlefield, then shuffle.";
const DRAGON_HATCHLING: &str = "Flying\n{R}: This creature gets +1/+0 until end of turn.";
const DISENCHANT: &str = "Destroy target artifact or enchantment.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const CHILD_OF_NIGHT: &str = "Lifelink";
// Vanilla (textless) filler body: there is no Oracle text to quote.
const GRIZZLY_BEARS: &str = "";
const MURDER: &str = "Destroy target creature.";

/// The bare chain sentinel rebound to the members the linked exile instruction
/// put into exile (CR 607.2a).
fn exiled_caused_sentinel() -> TargetFilter {
    TargetFilter::TrackedSetFiltered {
        id: TrackedSetId(0),
        filter: Box::new(TargetFilter::Any),
        caused_by: Some(ThisWayCause::Exiled),
    }
}

fn bare_sentinel() -> TargetFilter {
    TargetFilter::TrackedSet {
        id: TrackedSetId(0),
    }
}

fn parse(text: &str, name: &str, types: &[&str]) -> ParsedAbilities {
    let types: Vec<String> = types.iter().map(|s| (*s).to_string()).collect();
    parse_oracle_text(text, name, &[], &types, &[])
}

fn trigger_execute<'a>(
    parsed: &'a ParsedAbilities,
    name: &str,
    index: usize,
) -> &'a AbilityDefinition {
    parsed
        .triggers
        .get(index)
        .unwrap_or_else(|| panic!("{name} has a trigger[{index}]: {:#?}", parsed.triggers))
        .execute
        .as_deref()
        .unwrap_or_else(|| panic!("{name} trigger[{index}] has an execute"))
}

fn sub<'a>(def: &'a AbilityDefinition, what: &str) -> &'a AbilityDefinition {
    def.sub_ability
        .as_deref()
        .unwrap_or_else(|| panic!("{what} has a sub_ability: {def:#?}"))
}

fn sacrificed_then_zone(events: &[GameEvent], object: ObjectId, to: Zone) -> bool {
    let moved = events.iter().position(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: dest, .. }
                if *object_id == object && *dest == to
        )
    });
    let sacrificed = events.iter().position(|event| {
        matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == object
        )
    });
    matches!((moved, sacrificed), (Some(m), Some(s)) if m < s)
}

/// What `day_of_the_dragons_with_hatchling` returns.
struct DayBoard {
    day: ObjectId,
    hatchling: ObjectId,
    /// Child of Night, which Day's ETB exiles (only when the board has one).
    child: Option<ObjectId>,
    /// The events of the Disenchant drain.
    events: Vec<GameEvent>,
    runner: engine::game::scenario::GameRunner,
}

/// The stack drained and the game is back at a priority prompt (no stray prompt).
fn assert_settled(runner: &engine::game::scenario::GameRunner, what: &str) {
    let state = runner.state();
    assert!(state.stack.is_empty(), "{what}: the stack drained");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{what}: fully resolved, no stray prompt: {:?}",
        state.waiting_for
    );
}

fn battlefield_tokens(runner: &engine::game::scenario::GameRunner, name: &str) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|obj| obj.is_token && obj.name == name)
        })
        .collect()
}

/// Builds R1's board (optionally with Rest in Peace under P1 and Child of Night on
/// P0's battlefield), casts Day of the Dragons, then Dragon Hatchling, then Disenchant
/// on Day. With the child, Day's ETB exiles it and creates one 5/5 Dragon token.
fn day_of_the_dragons_with_hatchling(rest_in_peace: bool, with_child: bool) -> DayBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if rest_in_peace {
        scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    }
    let child = with_child.then(|| {
        scenario
            .add_creature_from_oracle(P0, "Child of Night", 2, 1, CHILD_OF_NIGHT)
            .with_subtypes(vec!["Vampire"])
            .id()
    });
    let day = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Day of the Dragons", DAY_OF_THE_DRAGONS)
        .id();
    let hatchling = scenario
        .add_creature_to_hand_from_oracle(P0, "Dragon Hatchling", 0, 1, DRAGON_HATCHLING)
        .with_subtypes(vec!["Dragon"])
        .id();
    let disenchant = scenario
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
        .id();
    let mut runner = scenario.build();

    let etb = runner.cast(day).resolve();
    // Reach guard (1): Day's ETB resolved. Over an empty board it exiled nothing and
    // created 0 tokens; with the child it exiled it and created one Dragon token.
    assert_eq!(
        etb.zone_of(day),
        Zone::Battlefield,
        "Day of the Dragons resolves onto the battlefield"
    );
    let dragon_tokens = etb
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_token && obj.zone == Zone::Battlefield)
        .count();
    assert_eq!(
        dragon_tokens,
        usize::from(with_child),
        "Day's ETB creates one token per exiled creature"
    );
    if let Some(child) = child {
        assert_eq!(
            etb.zone_of(child),
            Zone::Exile,
            "Day's ETB exiled Child of Night"
        );
    }

    let hatch = runner.cast(hatchling).resolve();
    // Reach guard (2): the nontoken Dragon card is on the battlefield before
    // Day leaves.
    assert_eq!(
        hatch.zone_of(hatchling),
        Zone::Battlefield,
        "Dragon Hatchling resolves onto the battlefield"
    );

    let removal = runner.cast(disenchant).target_object(day).resolve();
    let events = removal.events().to_vec();
    DayBoard {
        day,
        hatchling,
        child,
        events,
        runner,
    }
}

/// R1 — CR 607.2a + CR 406.6 + CR 701.21a: Day of the Dragons' LTB sacrifices
/// all Dragons, then returns "the exiled cards" — the cards its ETB exiled.
/// A sacrificed nontoken Dragon card is put into the graveyard by the sacrifice
/// (CR 701.21a), never into exile by the linked instruction, so it stays in the
/// graveyard. (This row asserts nothing about an ETB-exiled card; there is none.)
#[test]
fn day_of_the_dragons_ltb_does_not_return_a_sacrificed_dragon_card() {
    let DayBoard {
        day,
        hatchling,
        events,
        runner,
        ..
    } = day_of_the_dragons_with_hatchling(false, false);
    let state = runner.state();

    // Reach guard (3): the LTB fired and fully resolved, and its `Sacrifice`
    // clause ran, so the chain reached its return `sub_ability`.
    assert_eq!(
        state.objects[&day].zone,
        Zone::Graveyard,
        "Disenchant destroyed Day"
    );
    assert!(
        sacrificed_then_zone(&events, hatchling, Zone::Graveyard),
        "Day's LTB sacrificed Dragon Hatchling into the graveyard: {events:#?}"
    );
    assert!(state.stack.is_empty(), "the stack drained");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "fully resolved, no stray prompt: {:?}",
        state.waiting_for
    );

    assert_ne!(
        state.objects[&hatchling].zone,
        Zone::Battlefield,
        "the sacrificed Dragon card is not one of 'the exiled cards' (CR 607.2a)"
    );
    assert_eq!(state.objects[&hatchling].zone, Zone::Graveyard);
}

/// R9 / DR3 — CR 607.2a + CR 614.6 + CR 701.21a: under Rest in Peace the sacrifice
/// puts Dragon Hatchling into exile instead of the graveyard, but the card was
/// still sacrificed — never put into exile by Day's ETB instruction — so the
/// return leaves it in exile. `ExiledBySource` decides this row: the return scans
/// exile and reaches the card, but Rest in Peace's redirect of the sacrifice is not an
/// exile by Day's linked ETB (CR 607.2a + CR 614.6), so the card is not linked to Day and
/// stays. Child of Night, which Day's ETB did exile, returns.
#[test]
fn day_of_the_dragons_ltb_does_not_return_a_sacrificed_card_redirected_to_exile() {
    let DayBoard {
        day,
        hatchling,
        child,
        events,
        runner,
    } = day_of_the_dragons_with_hatchling(true, true);
    let child = child.expect("the board has Child of Night");
    let state = runner.state();

    // Reach guard (1), read from the drain's events (never the final zone):
    // the `Sacrifice` ran and Rest in Peace redirected it to exile.
    assert!(
        sacrificed_then_zone(&events, hatchling, Zone::Exile),
        "the sacrifice moved Dragon Hatchling Battlefield→Exile, then emitted \
         PermanentSacrificed: {events:#?}"
    );
    // Reach guard (2): Day left (to exile, under Rest in Peace) and its LTB fired.
    assert_eq!(
        state.objects[&day].zone,
        Zone::Exile,
        "Rest in Peace exiled Day"
    );
    // Reach guard (3).
    assert_settled(&runner, "DR3");

    assert_ne!(
        state.objects[&hatchling].zone,
        Zone::Battlefield,
        "a sacrificed card redirected to exile is not one of 'the exiled cards' (CR 607.2a)"
    );
    assert_eq!(state.objects[&hatchling].zone, Zone::Exile);
    assert_eq!(
        state.objects[&child].zone,
        Zone::Battlefield,
        "CR 607.2a: the card Day's ETB exiled returns"
    );
    assert_eq!(state.objects[&child].controller, P0);
}

/// DR1 — CR 607.2a: Day's LTB returns the card its ETB exiled (Child of Night)
/// under your control, and the sacrificed nontoken Dragon (Dragon Hatchling)
/// stays in the graveyard.
#[test]
fn day_of_the_dragons_ltb_returns_the_card_its_etb_exiled() {
    let DayBoard {
        hatchling,
        child,
        events,
        runner,
        ..
    } = day_of_the_dragons_with_hatchling(false, true);
    let child = child.expect("the board has Child of Night");
    let state = runner.state();

    // Reach guards: the LTB sacrificed Hatchling into the graveyard (every Dragon token went
    // with it) and the chain fully resolved.
    assert!(
        sacrificed_then_zone(&events, hatchling, Zone::Graveyard),
        "Day's LTB sacrificed Dragon Hatchling: {events:#?}"
    );
    assert!(
        battlefield_tokens(&runner, "Dragon").is_empty(),
        "the sacrifice took every Dragon token"
    );
    assert_settled(&runner, "DR1");

    assert_eq!(
        state.objects[&child].zone,
        Zone::Battlefield,
        "CR 607.2a: the card Day's ETB exiled returns"
    );
    assert_eq!(state.objects[&child].controller, P0, "under your control");
    assert_eq!(
        state.objects[&hatchling].zone,
        Zone::Graveyard,
        "the sacrificed Dragon card is not one of 'the exiled cards'"
    );
}

/// DR2 — ruling 2017-11-17: the exiled creature returns even when no Dragon token is left
/// when Day leaves the battlefield.
#[test]
fn day_of_the_dragons_ltb_returns_the_exiled_card_with_no_dragon_left() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let day = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Day of the Dragons", DAY_OF_THE_DRAGONS)
        .id();
    let child = scenario
        .add_creature_from_oracle(P0, "Child of Night", 2, 1, CHILD_OF_NIGHT)
        .with_subtypes(vec!["Vampire"])
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let disenchant = scenario
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
        .id();
    let mut runner = scenario.build();

    runner.cast(day).resolve();
    // Reach guards: the ETB exiled Child of Night and made exactly one Dragon token.
    assert_eq!(runner.state().objects[&child].zone, Zone::Exile);
    let dragons = battlefield_tokens(&runner, "Dragon");
    assert_eq!(dragons.len(), 1, "Day's ETB made one Dragon token");
    runner.cast(murder).target_object(dragons[0]).resolve();
    assert!(
        battlefield_tokens(&runner, "Dragon").is_empty(),
        "reach guard: no Dragon is left before Day leaves"
    );
    runner.cast(disenchant).target_object(day).resolve();

    assert_settled(&runner, "DR2");
    assert_eq!(runner.state().objects[&day].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&child].zone,
        Zone::Battlefield,
        "ruling 2017-11-17: the exiled creature returns with no Dragon left"
    );
}

/// DR4 — CR 607.2a: another source's exile is not one of "the exiled cards". P0's Oblivion
/// Ring holds P1's Grizzly Bears in exile; Day's LTB returns Child of Night (Day's own ETB
/// exile) and leaves the Bears there.
#[test]
fn day_of_the_dragons_ltb_does_not_return_another_sources_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oring = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Oblivion Ring", OBLIVION_RING)
        .id();
    let bears = scenario
        .add_creature_from_oracle(P1, "Grizzly Bears", 2, 2, GRIZZLY_BEARS)
        .id();
    let day = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Day of the Dragons", DAY_OF_THE_DRAGONS)
        .id();
    let child = scenario
        .add_creature_from_oracle(P0, "Child of Night", 2, 1, CHILD_OF_NIGHT)
        .with_subtypes(vec!["Vampire"])
        .id();
    let disenchant = scenario
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
        .id();
    let mut runner = scenario.build();

    runner.cast(oring).target_objects(&[bears]).resolve();
    runner.cast(day).resolve();
    // Reach guards: Oblivion Ring holds the Bears, Day's ETB holds Child of Night.
    assert_eq!(runner.state().objects[&oring].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&child].zone, Zone::Exile);
    runner.cast(disenchant).target_object(day).resolve();

    assert_settled(&runner, "DR4");
    assert_eq!(
        runner.state().objects[&child].zone,
        Zone::Battlefield,
        "CR 607.2a: the card Day's ETB exiled returns"
    );
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Exile,
        "CR 607.2a: Oblivion Ring's exile is not linked to Day"
    );
    assert_eq!(runner.state().objects[&bears].owner, P1);
    assert_eq!(
        runner.state().objects[&oring].zone,
        Zone::Battlefield,
        "Oblivion Ring is still on the battlefield"
    );
}

/// S1 — CR 607.2a + CR 400.3: The Spot's dies trigger returns both cards its ETB exiled to
/// their owners' hands (P1's permanent to P1's hand, P0's graveyard card to P0's hand). A
/// `Bounce` cannot take a card out of exile, so the return is a set-wide move out of exile.
#[test]
fn the_spot_dies_returns_the_exiled_cards_to_their_owners_hands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spot = scenario
        .add_creature_to_hand_from_oracle(P0, "The Spot, Living Portal", 4, 4, THE_SPOT)
        .id();
    let bears = scenario
        .add_creature_from_oracle(P1, "Grizzly Bears", 2, 2, GRIZZLY_BEARS)
        .id();
    let child = scenario
        .add_creature_to_graveyard(P0, "Child of Night", 2, 1)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    // A known filler card, so "the bottom of the library" is distinguishable from the top.
    scenario.with_library_top(P0, &["Grizzly Bears"]);
    let mut runner = scenario.build();

    runner.cast(spot).target_objects(&[bears, child]).resolve();
    // Reach guard: the ETB exiled both targets.
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&child].zone, Zone::Exile);
    runner.cast(murder).target_object(spot).resolve();

    assert_settled(&runner, "S1");
    let state = runner.state();
    assert_eq!(state.players[0].library.len(), 2);
    assert_eq!(
        state.players[0].library.last(),
        Some(&spot),
        "The Spot is on the bottom of its owner's library"
    );
    assert_ne!(
        state.players[0].library.front(),
        Some(&spot),
        "the filler card, not The Spot, is on top"
    );
    assert_eq!(state.objects[&bears].zone, Zone::Hand);
    assert!(
        state.players[1].hand.contains(&bears),
        "the Bears go to their owner's hand (CR 400.3)"
    );
    assert_eq!(state.objects[&child].zone, Zone::Hand);
    assert!(
        state.players[0].hand.contains(&child),
        "Child of Night goes to its owner's hand"
    );
}

/// R2 — Day of the Dragons' plural return, printed on the card, lowers to a set-wide
/// `ChangeZoneAll { origin: Exile }` over the source's linked exile (`ExiledBySource`,
/// CR 607.2a), never the single-pick `ChangeZone` route.
#[test]
fn day_of_the_dragons_return_binds_exiled_by_source_set_wide() {
    let parsed = parse(DAY_OF_THE_DRAGONS, "Day of the Dragons", &["Enchantment"]);

    let etb = trigger_execute(&parsed, "Day of the Dragons", 0);
    assert!(
        matches!(
            &*etb.effect,
            Effect::ChangeZoneAll {
                destination: Zone::Exile,
                ..
            }
        ),
        "Day's ETB exiles all creatures you control: {:?}",
        etb.effect
    );

    let ltb = trigger_execute(&parsed, "Day of the Dragons", 1);
    assert!(
        matches!(&*ltb.effect, Effect::Sacrifice { .. }),
        "the LTB chain starts with the sacrifice: {:?}",
        ltb.effect
    );
    let ret = sub(ltb, "Day's sacrifice");
    match &*ret.effect {
        Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            enters_under,
            ..
        } => {
            assert_eq!(*origin, Some(Zone::Exile));
            assert_eq!(*destination, Zone::Battlefield);
            assert_eq!(*target, TargetFilter::ExiledBySource);
            assert_eq!(*enters_under, Some(ControllerRef::You));
        }
        other => panic!("Day's return is a set-wide ChangeZoneAll, not {other:?}"),
    }
}

/// R3 — the quoted granted trigger is parsed with a fresh context: the parent's
/// "exile it" cannot reach it, so the granted chain's only prior clause is its
/// own sacrifice and "the exiled card" binds the `Exiled`-caused sentinel. The return is
/// written in an ability an effect creates (CR 607.1d), so it keeps D8's binding: this row
/// is the created-by-effect guard.
#[test]
fn timothar_granted_return_binds_exiled_caused_sentinel() {
    let parsed = parse(TIMOTHAR, "Timothar, Baron of Bats", &["Creature"]);
    let dies = trigger_execute(&parsed, "Timothar", 0);

    let mut parent_exiles = false;
    let mut granted = None;
    let mut cursor = Some(dies);
    while let Some(def) = cursor {
        match &*def.effect {
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            } => parent_exiles = true,
            Effect::GenericEffect {
                static_abilities, ..
            } => {
                granted = static_abilities
                    .iter()
                    .flat_map(|s| s.modifications.iter())
                    .find_map(|m| match m {
                        ContinuousModification::GrantTrigger { trigger } => Some(trigger.clone()),
                        _ => None,
                    });
            }
            _ => {}
        }
        cursor = def.sub_ability.as_deref();
    }
    // Positive reach guard: the hostile exile authority is present in the
    // parent ability.
    assert!(
        parent_exiles,
        "the parent chain exiles the dying Vampire: {dies:#?}"
    );
    let granted = granted.unwrap_or_else(|| panic!("the Bat gains a trigger: {dies:#?}"));
    let granted_exec = granted
        .execute
        .as_deref()
        .expect("the granted trigger has an execute");
    assert!(
        matches!(
            &*granted_exec.effect,
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "the granted chain sacrifices the Bat first: {:?}",
        granted_exec.effect
    );
    let ret = sub(granted_exec, "the granted sacrifice");
    match &*ret.effect {
        Effect::ChangeZone {
            origin,
            destination,
            target,
            enter_tapped,
            ..
        } => {
            assert_eq!(*origin, None);
            assert_eq!(*destination, Zone::Battlefield);
            assert_eq!(*target, exiled_caused_sentinel());
            assert_eq!(*enter_tapped, EtbTapState::Tapped);
        }
        other => panic!("the granted return is a ChangeZone, not {other:?}"),
    }
}

/// R4 — The Spot's plural hand return, printed on the card, lowers to a set-wide
/// `ChangeZoneAll` from exile to the owners' hands over the source's linked exile
/// (`ExiledBySource`, CR 607.2a), to their owners' hands (CR 400.3): `bounce::resolve` never moves a card out of exile.
/// The "If you do" gate stays.
#[test]
fn the_spot_dies_return_binds_exiled_by_source_set_wide() {
    let parsed = parse(THE_SPOT, "The Spot, Living Portal", &["Creature"]);
    let dies = trigger_execute(&parsed, "The Spot", 1);
    assert!(
        matches!(&*dies.effect, Effect::PutAtLibraryPosition { .. }),
        "the dies chain puts The Spot on the bottom first: {:?}",
        dies.effect
    );
    let ret = sub(dies, "The Spot's library put");
    assert_eq!(
        ret.condition,
        Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed
        }),
        "the return keeps its 'If you do' gate"
    );
    match &*ret.effect {
        Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            ..
        } => {
            assert_eq!(*origin, Some(Zone::Exile));
            assert_eq!(*destination, Zone::Hand);
            assert_eq!(*target, TargetFilter::ExiledBySource);
        }
        other => panic!("The Spot's return is a set-wide ChangeZoneAll, not {other:?}"),
    }
}

/// R5 — sibling guard: Oblivion Ring's LTB chain has no prior clause, so the
/// ETB/LTB linked pairing is unchanged (`ExiledBySource`).
#[test]
fn oblivion_ring_ltb_return_keeps_exiled_by_source_pairing() {
    let parsed = parse(OBLIVION_RING, "Oblivion Ring", &["Enchantment"]);
    let etb = trigger_execute(&parsed, "Oblivion Ring", 0);
    assert!(
        matches!(
            &*etb.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ),
        "the ETB half exiles: {:?}",
        etb.effect
    );
    let ltb = trigger_execute(&parsed, "Oblivion Ring", 1);
    match &*ltb.effect {
        Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            ..
        } => {
            assert_eq!(*origin, Some(Zone::Exile));
            assert_eq!(*destination, Zone::Battlefield);
            assert_eq!(*target, TargetFilter::ExiledBySource);
        }
        other => panic!("Oblivion Ring's LTB is the paired ChangeZoneAll, not {other:?}"),
    }
}

/// R6 — sibling guard: a same-chain (or enclosing-chain) exile producer keeps
/// the bare sentinel, including into a nested delayed body.
#[test]
fn same_or_enclosing_chain_exile_producer_keeps_bare_sentinel() {
    // (a) Sudden Disappearance: the delayed body's return.
    let parsed = parse(SUDDEN_DISAPPEARANCE, "Sudden Disappearance", &["Sorcery"]);
    let spell = parsed.abilities.first().expect("the spell ability");
    assert!(
        matches!(
            &*spell.effect,
            Effect::ChangeZoneAll {
                destination: Zone::Exile,
                ..
            }
        ),
        "the enclosing chain exiles first: {:?}",
        spell.effect
    );
    let delayed = sub(spell, "Sudden Disappearance's exile");
    match &*delayed.effect {
        Effect::CreateDelayedTrigger { effect, .. } => match &*effect.effect {
            Effect::ChangeZone {
                destination,
                target,
                ..
            } => {
                assert_eq!(*destination, Zone::Battlefield);
                assert_eq!(*target, bare_sentinel());
            }
            other => panic!("the delayed return is a ChangeZone, not {other:?}"),
        },
        other => panic!("the return is a delayed trigger, not {other:?}"),
    }

    // (b) Indomitable Creativity: the put return after a kept-in-exile reveal.
    let parsed = parse(
        INDOMITABLE_CREATIVITY,
        "Indomitable Creativity",
        &["Sorcery"],
    );
    let spell = parsed.abilities.first().expect("the spell ability");
    let reveal = sub(spell, "Indomitable Creativity's destroy");
    assert!(
        matches!(
            &*reveal.effect,
            Effect::RevealUntil {
                kept_destination: Zone::Exile,
                ..
            }
        ),
        "the chain keeps the revealed card in exile: {:?}",
        reveal.effect
    );
    let put = sub(reveal, "Indomitable Creativity's reveal");
    match &*put.effect {
        Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            ..
        } => {
            assert_eq!(*origin, Some(Zone::Exile));
            assert_eq!(*destination, Zone::Battlefield);
            assert_eq!(*target, bare_sentinel());
        }
        other => panic!("the put return is a ChangeZoneAll, not {other:?}"),
    }
}

/// R8 — sibling guard: Tidehollow Sculler's singular hand return keeps the
/// CR 607.2a `ExiledBySource` carve-out.
#[test]
fn tidehollow_sculler_ltb_return_keeps_exiled_by_source() {
    let parsed = parse(
        TIDEHOLLOW_SCULLER,
        "Tidehollow Sculler",
        &["Artifact", "Creature"],
    );
    let etb = trigger_execute(&parsed, "Tidehollow Sculler", 0);
    let exile = sub(etb, "Tidehollow Sculler's reveal");
    assert!(
        matches!(
            &*exile.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "the ETB exiles the chosen card: {:?}",
        exile.effect
    );
    let ltb = trigger_execute(&parsed, "Tidehollow Sculler", 1);
    match &*ltb.effect {
        Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            ..
        } => {
            assert_eq!(*origin, Some(Zone::Exile));
            assert_eq!(*destination, Zone::Hand);
            assert_eq!(*target, TargetFilter::ExiledBySource);
        }
        other => panic!("Tidehollow Sculler's LTB is a ChangeZoneAll, not {other:?}"),
    }
}

/// R10 — fence: a delayed-trigger payload keeps its bare `TrackedSet` binding
/// even when the creating chain's earlier clauses are non-exile publishers.
/// CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, and the later obligations (incarnation pins, consumed-set release) key on the bare `TrackedSet` shape. The parser decides "the exiled card(s)" once, on the finished tree (`bind_linked_exile_returns`), from the instructions before the return on its own resolution line, in the order written (CR 608.2c). A `CreateDelayedTrigger` payload is not entered by that walk, so a return inside it keeps the bare sentinel, whatever the creating chain or the payload's own earlier clauses publish, and wherever assembly or a later pass put it.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden
/// Disappearance, whose creating chain has an exile producer (so its delayed
/// return keeps the bare sentinel, row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn delayed_return_under_non_exile_creating_chain_keeps_bare_sentinel() {
    let parsed = parse(
        SYNTHETIC_DELAYED_RETURN_AFTER_SACRIFICE,
        "Synthetic Delayed Return",
        &["Enchantment"],
    );
    let ltb = trigger_execute(&parsed, "the synthetic face", 0);
    // Reach guard: the creating chain's first clause is the non-exile sacrifice.
    assert!(
        matches!(&*ltb.effect, Effect::Sacrifice { .. }),
        "the creating chain sacrifices first: {:?}",
        ltb.effect
    );
    let delayed = sub(ltb, "the sacrifice");
    match &*delayed.effect {
        Effect::CreateDelayedTrigger { effect, .. } => match &*effect.effect {
            Effect::ChangeZone {
                destination,
                target,
                ..
            } => {
                assert_eq!(*destination, Zone::Battlefield);
                assert_eq!(*target, bare_sentinel());
            }
            other => panic!("the delayed return is a ChangeZone, not {other:?}"),
        },
        other => panic!("the return is a delayed trigger, not {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Shape rows R11-R118.
//
// Each row parses its synthetic face with the production `parse_oracle_text`
// and compares the finished tree, rendered in the notation below, to the shape
// the plan measured. The rendering walks every root (`T<i>` trigger executes,
// `A<i>` abilities), each chain's `sub_ability`, `else_ability` and
// `mode_abilities`, and expands a `CreateDelayedTrigger` payload as
// `CDT{<condition>}[...]`, so a row's expected string is also its reach guard:
// it only matches when the return sits at the node the row names, with the
// conditions and flags the row names on its path.
//
// Notation: `?Cond` is the node's condition type, `else:(...)` its
// `else_ability`, `modeN:(...)` its modes, `TSF{0,Any,Exiled}` the created-by-effect (D8)
// binding (`exiled_caused_sentinel()`), `ExiledBySource` the printed-host binding,
// `TrackedSet(0)` the bare sentinel.
// Every `E` face also carries `T0: ChangeZoneAll{None,Exile,Typed}`, the ETB
// exile whose cards the return names.
// ---------------------------------------------------------------------------

/// Counters the renderer fills while it walks.
#[derive(Default)]
struct PayloadCounts {
    /// A linked-exile return binding (D8's `TrackedSetFiltered { caused_by: Exiled }` or the
    /// printed-host `ExiledBySource`) on a creation-bound payload edge must never happen
    /// (CR 603.7c).
    linked_binding_in_payload: usize,
    /// Battlefield returns on a creation-bound payload edge.
    returns_in_payload: usize,
}

fn json_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(fields)) => !fields.is_empty(),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::Bool(true)) => true,
    }
}

fn json_plain(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn json_type(value: Option<&Value>) -> String {
    json_plain(value.and_then(|v| v.get("type")))
}

fn target_shape(target: Option<&Value>) -> String {
    match target {
        Some(Value::Object(fields)) => match fields.get("type").and_then(Value::as_str) {
            Some("TrackedSet") => format!("TrackedSet({})", json_plain(fields.get("id"))),
            Some("TrackedSetFiltered") => format!(
                "TSF{{{},{},{}}}",
                json_plain(fields.get("id")),
                json_type(fields.get("filter")),
                json_plain(fields.get("caused_by"))
            ),
            Some(other) => other.to_string(),
            None => "None".to_string(),
        },
        other => json_plain(other),
    }
}

fn effect_shape(effect: &Value, in_payload: bool, counts: &mut PayloadCounts) -> String {
    let kind = json_type(Some(effect));
    match kind.as_str() {
        "ChangeZone" | "ChangeZoneAll" | "Bounce" => {
            let target = effect.get("target");
            if in_payload {
                if target
                    .and_then(|t| t.get("caused_by"))
                    .and_then(Value::as_str)
                    == Some("Exiled")
                    || json_type(target) == "ExiledBySource"
                {
                    counts.linked_binding_in_payload += 1;
                }
                if kind != "Bounce"
                    && effect.get("destination").and_then(Value::as_str) == Some("Battlefield")
                {
                    counts.returns_in_payload += 1;
                }
            }
            format!(
                "{kind}{{{},{},{}}}",
                json_plain(effect.get("origin")),
                json_plain(effect.get("destination")),
                target_shape(target)
            )
        }
        "CreateDelayedTrigger" => format!(
            "CDT{{{}}}[{}]",
            json_type(effect.get("condition")),
            chain_shape(effect.get("effect"), true, counts)
        ),
        "FlipCoins" => {
            let branches: Vec<String> = [("win", "win_effect"), ("los", "lose_effect")]
                .iter()
                .filter(|(_, key)| json_truthy(effect.get(*key)))
                .map(|(label, key)| {
                    format!(
                        "{label}:{}",
                        chain_shape(effect.get(*key), in_payload, counts)
                    )
                })
                .collect();
            format!("FlipCoins({})", branches.join(" / "))
        }
        "Unimplemented" => format!("Unimpl({})", json_plain(effect.get("name"))),
        _ => kind,
    }
}

fn chain_shape(def: Option<&Value>, in_payload: bool, counts: &mut PayloadCounts) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = def.filter(|d| json_truthy(Some(d)));
    while let Some(def) = current {
        let mut part = effect_shape(
            def.get("effect").unwrap_or(&Value::Null),
            in_payload,
            counts,
        );
        if json_truthy(def.get("condition")) {
            part.push('?');
            part.push_str(&json_type(def.get("condition")));
        }
        parts.push(part);
        if json_truthy(def.get("else_ability")) {
            parts.push(format!(
                "else:({})",
                chain_shape(def.get("else_ability"), in_payload, counts)
            ));
        }
        if let Some(Value::Array(modes)) = def.get("mode_abilities") {
            for (index, mode) in modes.iter().enumerate() {
                parts.push(format!(
                    "mode{index}:({})",
                    chain_shape(Some(mode), in_payload, counts)
                ));
            }
        }
        current = def.get("sub_ability").filter(|d| json_truthy(Some(d)));
    }
    parts.join(", ")
}

/// Every root chain of a parsed face: trigger executes, then abilities.
fn root_chains(parsed: &Value) -> Vec<(String, Option<&Value>)> {
    let mut roots = Vec::new();
    if let Some(Value::Array(triggers)) = parsed.get("triggers") {
        for (index, trigger) in triggers.iter().enumerate() {
            roots.push((format!("T{index}"), trigger.get("execute")));
        }
    }
    if let Some(Value::Array(abilities)) = parsed.get("abilities") {
        for (index, ability) in abilities.iter().enumerate() {
            roots.push((format!("A{index}"), Some(ability)));
        }
    }
    roots
}

/// Renders a parsed face as the row notation; `counts` receives the payload
/// counters.
fn face_shape(parsed: &Value, counts: &mut PayloadCounts) -> String {
    root_chains(parsed)
        .into_iter()
        .map(|(label, chain)| format!("{label}: {}", chain_shape(chain, false, counts)))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The decision-bearing fields a row's reach guard asserts (`optional`,
/// `optional_for`, `unless_pay`, `player_scope`, `repeat_for`, ...), as
/// `field@position:EffectType` in chain order. `shapes` does not print them.
fn face_flags(parsed: &Value) -> String {
    const FLAGS: [&str; 7] = [
        "optional",
        "optional_player",
        "optional_for",
        "unless_pay",
        "player_scope",
        "repeat_for",
        "repeat_until",
    ];
    fn walk(def: Option<&Value>, depth: usize, out: &mut Vec<String>) {
        let Some(def) = def.filter(|d| json_truthy(Some(d))) else {
            return;
        };
        for flag in FLAGS {
            if json_truthy(def.get(flag)) {
                out.push(format!("{flag}@{depth}:{}", json_type(def.get("effect"))));
            }
        }
        walk(def.get("else_ability"), depth + 100, out);
        walk(def.get("sub_ability"), depth + 1, out);
    }
    let mut out = Vec::new();
    for (_, chain) in root_chains(parsed) {
        walk(chain, 0, &mut out);
    }
    out.join(",")
}

/// Every definition reachable through `sub_ability`, `else_ability` and
/// `mode_abilities` (not through a delayed-trigger payload).
fn collect_definitions<'a>(def: Option<&'a Value>, out: &mut Vec<&'a Value>) {
    let Some(def) = def.filter(|d| json_truthy(Some(d))) else {
        return;
    };
    out.push(def);
    collect_definitions(def.get("sub_ability"), out);
    collect_definitions(def.get("else_ability"), out);
    if let Some(Value::Array(modes)) = def.get("mode_abilities") {
        for mode in modes {
            collect_definitions(Some(mode), out);
        }
    }
}

/// CR 118.12: the "If you can't" gate, `Not(ZoneChangedThisWay { filter: Any })`,
/// is not the admitted "If you do" gate.
fn has_cant_gate_on_a_battlefield_return(parsed: &Value) -> bool {
    let cant_gate = serde_json::json!({
        "type": "Not",
        "condition": { "type": "ZoneChangedThisWay", "filter": { "type": "Any" } },
    });
    let mut defs = Vec::new();
    for (_, chain) in root_chains(parsed) {
        collect_definitions(chain, &mut defs);
    }
    defs.into_iter().any(|def| {
        json_type(def.get("effect")) == "ChangeZone"
            && def
                .get("effect")
                .and_then(|e| e.get("destination"))
                .and_then(Value::as_str)
                == Some("Battlefield")
            && def.get("condition") == Some(&cant_gate)
    })
}

fn parse_row(id: &str, text: &str, types: &[&str]) -> Value {
    let parsed = parse(text, &format!("Synthetic {id}"), types);
    serde_json::to_value(&parsed).expect("ParsedAbilities serializes")
}

/// `Effect::Unimplemented` nodes in the parsed face (the parser's honest
/// strict-failure marker).
fn unimplemented_nodes(parsed: &Value) -> usize {
    parsed.to_string().matches("Unimplemented").count()
}

/// A fence row: the return sits inside a `CreateDelayedTrigger` payload and
/// keeps START's payload shape (CR 603.7c); no payload edge gains a linked-exile binding.
fn assert_fence_row(id: &str, text: &str, types: &[&str], shape: &str, unimplemented: usize) {
    let parsed = parse_row(id, text, types);
    let mut counts = PayloadCounts::default();
    assert_eq!(face_shape(&parsed, &mut counts), shape, "{id}: tree shape");
    assert_eq!(
        unimplemented_nodes(&parsed),
        unimplemented,
        "{id}: unimplemented nodes"
    );
    assert_eq!(
        counts.linked_binding_in_payload, 0,
        "{id}: a linked-exile binding inside a payload"
    );
    // Reach guard: the return really is inside a payload.
    assert!(
        counts.returns_in_payload >= 1,
        "{id}: the return is on a payload edge"
    );
}

/// A keep row: the return sits outside every payload. `shape` names the node
/// that holds it and its binding (the linked-exile binding on the line: `ExiledBySource` for a
/// printed return, D8's `TrackedSetFiltered { caused_by: Exiled }` for a created-by-effect
/// return; START's bare sentinel off it); `flags` is the decision-bearing field list on its path.
fn assert_keep_row(
    id: &str,
    text: &str,
    types: &[&str],
    shape: &str,
    unimplemented: usize,
    flags: &str,
    cant_gate: bool,
) {
    let parsed = parse_row(id, text, types);
    let mut counts = PayloadCounts::default();
    assert_eq!(face_shape(&parsed, &mut counts), shape, "{id}: tree shape");
    assert_eq!(
        unimplemented_nodes(&parsed),
        unimplemented,
        "{id}: unimplemented nodes"
    );
    assert_eq!(
        face_flags(&parsed),
        flags,
        "{id}: decision flags on the path"
    );
    assert_eq!(
        counts.linked_binding_in_payload, 0,
        "{id}: a linked-exile binding inside a payload"
    );
    assert_eq!(counts.returns_in_payload, 0, "{id}: payload returns");
    if cant_gate {
        assert!(
            has_cant_gate_on_a_battlefield_return(&parsed),
            "{id}: the return carries the \"If you can't\" gate"
        );
    }
}

// Row faces: synthetic, byte-identical to the plan's row table.
const R11_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a 1/1 white Spirit creature token with flying. Return the exiled cards to the battlefield under your control.";
const R12_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, you gain 3 life, then return the exiled cards to the battlefield under your control.";
const R13_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You gain 3 life at the beginning of the next end step, then return the exiled cards to the battlefield under your control.";
const R14_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, tap target creature. When that creature leaves the battlefield this turn, return the exiled cards to the battlefield under your control.";
const R15_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, at the beginning of the next end step, sacrifice all Dragons you control, then return the exiled cards to the battlefield under your control.";
const R16_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice a Dragon. At the beginning of the next end step, create a Treasure token. Return the exiled card to the battlefield under your control.";
const R17_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, at the beginning of the next end step, create a 1/1 white Spirit creature token with flying. Return the exiled cards to the battlefield under your control.";
const R18_FACE: &str = "At the beginning of the next end step, sacrifice all Dragons you control, then return the exiled cards to the battlefield under your control.";
const R19_FACE: &str = "Whenever a creature you control dies this turn, draw a card, then return the exiled cards to the battlefield under your control.";
const R20_FACE: &str = "When you next cast a creature spell this turn, draw a card, then return the exiled cards to the battlefield under your control.";
const R21_FACE: &str = "Flip a coin. When you win the flip, draw a card, then return the exiled cards to the battlefield under your control.";
const R22_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, tap target creature. When that creature dies this turn, draw a card, then return the exiled cards to the battlefield under your control.";
const R23_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, remove a counter from it. When you remove a counter this way, choose one \u{2014}\n\u{2022} Draw a card, then return the exiled cards to the battlefield under your control.\n\u{2022} You gain 3 life.";
const R24_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, draw a card. If you do, return the exiled cards to the battlefield under your control.";
const R25_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, you gain 3 life. Return the exiled cards to the battlefield under your control.";
const R26_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. When you do, return the exiled cards to the battlefield under your control.";
const R27_FACE: &str = "When you next attack this turn, draw a card, then return the exiled cards to the battlefield under your control.";
const R28_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. When you next attack this turn, draw a card, then return the exiled cards to the battlefield under your control.";
const R29_FACE: &str = "This spell deals 3 damage to target creature. When excess damage is dealt to the creature this way, draw a card, then return the exiled cards to the battlefield under your control.";
const R30_FACE: &str = "Search your library for a basic land card, reveal it, put it into your hand, then shuffle. When you search your library this way, draw a card, then return the exiled cards to the battlefield under your control.";
const R31_FACE: &str = "When this enchantment enters, exile all creatures you control.\nYou may reveal this card from your opening hand. If you do, at the beginning of the first upkeep, draw a card, then return the exiled cards to the battlefield under your control.";
const R32_FACE: &str = "At the beginning of the next end step, draw a card. If you control a Dragon, instead draw two cards and return the exiled cards to the battlefield under your control.";
const R33_FACE: &str = "Whenever a creature you control dies this turn, draw a card. If you control a Dragon, instead draw two cards and return the exiled cards to the battlefield under your control.";
const R34_FACE: &str = "Whenever a creature you control dies this turn, draw a card. If you control a Dragon, instead draw two cards, then return the exiled cards to the battlefield under your control.";
const R35_FACE: &str = "At the beginning of the next end step, draw a card. If you control a Dragon, instead draw two cards, then return the exiled cards to the battlefield under your control.";
const R36_FACE: &str = "At the beginning of the next end step, until end of turn, your opponents can't cast spells, you sacrifice a creature, then you return the exiled cards to the battlefield under your control.";
const R37_FACE: &str = "Whenever a creature you control dies this turn, until end of turn, your opponents can't cast spells, you sacrifice a creature, and you return the exiled cards to the battlefield under your control.";
const R38_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, at the beginning of the next end step, until end of turn, your opponents can't cast spells, you sacrifice a creature, and you return the exiled cards to the battlefield under your control.";
const R39_FACE: &str = "At the beginning of the next end step, for each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R40_FACE: &str = "At the beginning of the next end step, until end of turn, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, and you return the exiled cards to the battlefield under your control.";
const R41_FACE: &str = "At the beginning of the next end step, for each opponent, creatures you control get +1/+1 until end of turn and gain trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R42_FACE: &str = "Whenever a creature you control dies this turn, for each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R43_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, at the beginning of the next end step, for each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R44_FACE: &str = "When you next attack this turn, for each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R45_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, tap target creature. When that creature dies this turn, for each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R46_FACE: &str = "When you next cast a creature spell this turn, for each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R47_FACE: &str = "Search your library for a basic land card, reveal it, put it into your hand, then shuffle. When you search your library this way, for each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R48_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a Treasure token. For each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R49_FACE: &str = "At the beginning of the next end step, you may sacrifice a creature. If you do, for each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R50_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You gain 3 life at the beginning of the next end step if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R51_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, you gain 2 life if you control a Dragon. At the beginning of the next end step, create a Treasure token. Draw a card. Otherwise, return the exiled cards to the battlefield under your control.";
const R52_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a Treasure token. Draw a card. Otherwise, return the exiled cards to the battlefield under your control.";
const R53_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You gain 3 life at the beginning of the next end step. If you control a Dragon, instead draw two cards, then return the exiled cards to the battlefield under your control.";
const R54_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You gain 3 life at the beginning of the next end step if you control a Dragon. Otherwise, draw a card, then return the exiled cards to the battlefield under your control.";
const R55_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You gain 2 life if you control a Dragon. At the beginning of the next end step, create a Treasure token. Draw a card. Otherwise, return the exiled cards to the battlefield under your control.";
const R56_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, you gain 2 life if you control a Dragon. At the beginning of the next end step, create a Treasure token. Creatures you control gain flying until end of turn if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R57_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, any opponent may have you draw a card. At the beginning of the next end step, create a Treasure token. Draw a card. If no one does, return the exiled cards to the battlefield under your control.";
const R58_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Tap target permanent at the beginning of the next end step. If that permanent has flying, return the exiled cards to the battlefield under your control instead.";
const R59_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You gain 3 life at the beginning of the next end step, then draw a card if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R60_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a Treasure token. Draw a card if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R61_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a 1/1 red Elemental creature token. It deals 1 damage to any target. If you control a Dragon, instead it deals twice that much damage to that permanent or player, then return the exiled cards to the battlefield under your control.";
const R62_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a Treasure token. Any opponent may have you draw a card. If no one does, return the exiled cards to the battlefield under your control.";
const R63_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a Treasure token. Tap target permanent. If that permanent has flying, return the exiled cards to the battlefield under your control instead.";
const R64_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, you gain 2 life if you control a Dragon. At the beginning of the next end step, create a Treasure token. Create two 1/1 white Soldier creature tokens. If you control a Dragon, they enter tapped and attacking. Otherwise, return the exiled cards to the battlefield under your control.";
const R65_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a Treasure token. Create two 1/1 white Soldier creature tokens. If you control a Dragon, they enter tapped and attacking. Otherwise, return the exiled cards to the battlefield under your control.";
const R66_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a 1/1 red Elemental creature token. It deals 1 damage to any target. It deals 1 damage to you at the beginning of your next upkeep. If you control a Dragon, instead it deals twice that much damage to that permanent or player, then return the exiled cards to the battlefield under your control.";
const R67_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a Treasure token. Draw a card if you control a Dragon. Otherwise, you gain 1 life. Create two 1/1 white Soldier creature tokens. If you control a Dragon, they enter tapped and attacking. Otherwise, return the exiled cards to the battlefield under your control.";
const R68_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. At the beginning of the next end step, create a Treasure token. Put a flying counter on target creature you control if you control a Dragon. Repeat this process for first strike and trample. Otherwise, return the exiled cards to the battlefield under your control.";
const R69_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, whenever a creature you control dies this turn, create a Treasure token. Sacrifice it at the beginning of the next end step if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R70_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, whenever a creature you control dies this turn, create a Treasure token. Sacrifice it at the beginning of the next end step, reveal the top card of your library if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R71_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, whenever a creature you control dies this turn, create a Treasure token. Sacrifice it at the beginning of the next end step, flip a coin. If you win the flip, return the exiled cards to the battlefield under your control.";
const R72_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, whenever a creature you control dies this turn, create a Treasure token. Sacrifice it at the beginning of the next end step, flip a coin. If you lose the flip, draw a card. If you win the flip, return the exiled cards to the battlefield under your control.";
const R73_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, whenever a creature you control dies this turn, create a Treasure token. Sacrifice it at the beginning of the next end step, reveal the top three cards of your library if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R74_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, whenever a creature you control dies this turn, create a Treasure token. Sacrifice it at the beginning of the next end step, draw a card if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R75_FACE: &str = "Draw a card. If you control a Dragon, instead return the exiled cards to the battlefield under your control.";
const R76_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Tap target creature and return the exiled card to the battlefield under your control.";
const R77_FACE: &str = "Draw a card if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R78_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Exile target creature if you control a Dragon. Otherwise, return the exiled cards to the battlefield under your control.";
const R80_FACE: &str = "Exile target creature. If you control a Dragon, instead draw two cards. Return the exiled cards to the battlefield under your control.";
const R83_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Any opponent may have you exile target creature. If no one does, return the exiled cards to the battlefield under your control.";
const R84_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, any opponent may have you draw a card. If no one does, return the exiled cards to the battlefield under your control.";
const R85_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You may exile target creature. If you don't, return the exiled cards to the battlefield under your control.";
const R86_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You may exile target creature. If you do, draw a card. Otherwise, return the exiled cards to the battlefield under your control.";
const R87_FACE: &str = "You may sacrifice a creature. If you do, draw a card. Otherwise, return the exiled cards to the battlefield under your control.";
const R88_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You may exile target creature. If you don't, draw a card. Return the exiled cards to the battlefield under your control.";
const R89_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You may exile target creature. If you don't, draw a card and you gain 3 life. Return the exiled cards to the battlefield under your control.";
const R93_FACE: &str = "Draw a card unless target opponent pays 3 life. If they do, return the exiled cards to the battlefield under your control.";
const R94_FACE: &str = "Draw a card unless target opponent pays 3 life. If they don't, return the exiled cards to the battlefield under your control.";
const R95_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Each opponent may exile a creature they control. If they don't, return the exiled cards to the battlefield under your control.";
const R96_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Any opponent may have you exile target creature. If a player does, draw a card. If no one does, return the exiled cards to the battlefield under your control.";
const R97_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, any opponent may have you draw a card. If a player does, you gain 3 life. If no one does, return the exiled cards to the battlefield under your control.";
const R98_FACE: &str =
    "You may pay {2}. If you don't, return the exiled cards to the battlefield under your control.";
const R99_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You may exile target creature. If you do, you may draw a card. If you don't, return the exiled cards to the battlefield under your control.";
const R100_FACE: &str = "You may draw a card. If you do, you may sacrifice a creature. If you don't, return the exiled cards to the battlefield under your control.";
const R101_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You may exile target creature. If you do, you may draw a card. If you do, you gain 3 life. Otherwise, return the exiled cards to the battlefield under your control.";
const R102_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You may exile target creature. If you do, each opponent may draw a card. For each opponent who doesn't, return the exiled cards to the battlefield under your control.";
const R103_FACE: &str = "You may sacrifice a creature. If you do, each opponent may sacrifice a creature. For each opponent who doesn't, return the exiled cards to the battlefield under your control.";
const R105_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice a creature. If you can't, return the exiled cards to the battlefield under your control.";
const R106_FACE: &str = "Sacrifice a creature. If you can't, return the exiled cards to the battlefield under your control.";
const R107_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Exile target creature you control. If you can't, return the exiled cards to the battlefield under your control.";
const R109_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. You may exile target creature. If you do, sacrifice a creature. If you can't, return the exiled cards to the battlefield under your control.";
const R110_FACE: &str = "Sacrifice a creature unless target opponent pays 3 life. If you can't, return the exiled cards to the battlefield under your control.";
const R112_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice a creature. If you do, return the exiled cards to the battlefield under your control.";
const R113_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, flip a coin. If you win the flip, return the exiled cards to the battlefield under your control.";
const R114_FACE: &str = "Draw a card. You gain 2 life. If you control a Dragon, return the exiled cards to the battlefield under your control.";
const R115_FACE: &str = "You may sacrifice a creature. If you do, return the exiled cards to the battlefield under your control.";
const R116_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, at the beginning of the next end step, choose one \u{2014}\n\u{2022} Draw a card, then return the exiled cards to the battlefield under your control.\n\u{2022} You gain 3 life.";
const R117_FACE: &str = "Sacrifice a creature. For each opponent, target creature gets +2/+2 until end of turn and gains trample, you sacrifice a creature, then return the exiled cards to the battlefield under your control.";
const R118_FACE: &str = "When this enchantment enters, exile all creatures you control.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Each opponent loses 2 life, then return the exiled cards to the battlefield under your control.";

/// R11 — fence (face `p2:f b`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r11_p2_f_b_keeps_the_payload_return_bare() {
    assert_fence_row("R11", R11_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R12 — fence (face `p2:f c`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r12_p2_f_c_keeps_the_payload_return_bare() {
    assert_fence_row("R12", R12_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[GainLife, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R13 — fence (face `p2:f d`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r13_p2_f_d_keeps_the_payload_return_bare() {
    assert_fence_row("R13", R13_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[GainLife, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R14 — fence (face `p2:f e`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r14_p2_f_e_keeps_the_payload_return_bare() {
    assert_fence_row("R14", R14_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: SetTapState, CDT{WhenLeavesPlayFiltered}[ChangeZone{None,Battlefield,ParentTarget}]", 0);
}

/// R15 — fence (face `p2:f f`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r15_p2_f_f_keeps_the_payload_return_bare() {
    assert_fence_row("R15", R15_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{AtNextPhase}[Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R16 — fence (face `p2:f g`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r16_p2_f_g_keeps_the_payload_return_bare() {
    assert_fence_row("R16", R16_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R17 — fence (face `p2:f h`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r17_p2_f_h_keeps_the_payload_return_bare() {
    assert_fence_row("R17", R17_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{AtNextPhase}[Token, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R18 — fence (face `p2:f v1`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r18_p2_f_v1_keeps_the_payload_return_bare() {
    assert_fence_row(
        "R18",
        R18_FACE,
        &["Sorcery"],
        "A0: CDT{AtNextPhase}[Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]",
        0,
    );
}

/// R19 — fence (face `p2:f w2`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r19_p2_f_w2_keeps_the_payload_return_bare() {
    assert_fence_row(
        "R19",
        R19_FACE,
        &["Sorcery"],
        "A0: CDT{WheneverEvent}[Draw, ChangeZone{None,Battlefield,TrackedSet(0)}]",
        0,
    );
}

/// R20 — fence (face `p2:f n2`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r20_p2_f_n2_keeps_the_payload_return_bare() {
    assert_fence_row(
        "R20",
        R20_FACE,
        &["Sorcery"],
        "A0: CDT{WhenNextEvent}[Draw, ChangeZone{None,Battlefield,TrackedSet(0)}]",
        0,
    );
}

/// R21 — fence (face `p2:f cf`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r21_p2_f_cf_keeps_the_payload_return_bare() {
    assert_fence_row(
        "R21",
        R21_FACE,
        &["Sorcery"],
        "A0: FlipCoins(), CDT{WhenNextEvent}[Draw, ChangeZone{None,Battlefield,TrackedSet(0)}]",
        0,
    );
}

/// R22 — fence (face `p2:f in`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r22_p2_f_in_keeps_the_payload_return_bare() {
    assert_fence_row("R22", R22_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: SetTapState, CDT{WhenDies}[Draw, ChangeZone{None,Battlefield,ParentTarget}]", 0);
}

/// R23 — fence (face `p2:f mr`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r23_p2_f_mr_keeps_the_payload_return_bare() {
    assert_fence_row("R23", R23_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: RemoveCounter, CDT{WhenNextEvent}[GenericEffect, mode0:(Draw, ChangeZone{None,Battlefield,TrackedSet(0)}), mode1:(GainLife)]", 0);
}

/// R24 — fence (face `p2:f v10`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r24_p2_f_v10_keeps_the_payload_return_bare() {
    assert_fence_row("R24", R24_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Draw, ChangeZone{None,Battlefield,TrackedSet(0)}?EffectOutcome]", 0);
}

/// R25 — keep (face `p2:f v2`): the return binds the source's linked exile (`ExiledBySource`, the printed-host binding) on the resolution line. The sacrifice and the `CreateDelayedTrigger` definition precede the return on the line, and the payload is not an instruction of this chain (CR 603.7a), so the return binds the source's linked exile (`ExiledBySource`, the printed-host binding) (CR 607.2a).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r25_p2_f_v2_binds_exiled_by_source() {
    assert_keep_row("R25", R25_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[GainLife], ChangeZoneAll{Exile,Battlefield,ExiledBySource}", 0, "", false);
}

/// R26 — keep (face `p2:f v4`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: the return's own `WhenYouDo`. "When you do" is a reflexive trigger (CR 603.12), not the admitted gate. No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r26_p2_f_v4_binds_bare() {
    assert_keep_row("R26", R26_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}?WhenYouDo", 0, "", false);
}

/// R27 — fence (face `p2:p6 next attack`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r27_p2_p6_next_attack_keeps_the_payload_return_bare() {
    assert_fence_row(
        "R27",
        R27_FACE,
        &["Sorcery"],
        "A0: CDT{WhenNextEvent}[Draw, ChangeZone{None,Battlefield,TrackedSet(0)}]",
        0,
    );
}

/// R28 — fence (face `p2:p6b next attack ltb`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r28_p2_p6b_next_attack_ltb_keeps_the_payload_return_bare() {
    assert_fence_row("R28", R28_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{WhenNextEvent}[Draw, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R29 — fence (face `p2:p7 excess this way`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r29_p2_p7_excess_this_way_keeps_the_payload_return_bare() {
    assert_fence_row(
        "R29",
        R29_FACE,
        &["Sorcery"],
        "A0: DealDamage, CDT{WhenNextEvent}[Draw, ChangeZone{None,Battlefield,TrackedSet(0)}]",
        0,
    );
}

/// R30 — fence (face `p2:p7b search this way`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r30_p2_p7b_search_this_way_keeps_the_payload_return_bare() {
    assert_fence_row("R30", R30_FACE, &["Sorcery"], "A0: SearchLibrary, ChangeZone{Library,Hand,Any}, Shuffle, CDT{WhenNextEvent}[Draw, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R31 — fence (face `p2:p10 opening hand`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r31_p2_p10_opening_hand_keeps_the_payload_return_bare() {
    assert_fence_row("R31", R31_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | A0: Reveal, CDT{AtNextPhase}[Draw, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R32 — fence (face `p2:q4 next end instead and`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r32_p2_q4_next_end_instead_and_keeps_the_payload_return_bare() {
    assert_fence_row("R32", R32_FACE, &["Sorcery"], "A0: CDT{AtNextPhase}[Draw, Draw?ConditionInstead, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R33 — fence (face `p2:q2 whenever instead and`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r33_p2_q2_whenever_instead_and_keeps_the_payload_return_bare() {
    assert_fence_row("R33", R33_FACE, &["Sorcery"], "A0: CDT{WheneverEvent}[Draw, Draw?ConditionInstead, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R34 — fence (face `p2:q3 whenever instead then`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r34_p2_q3_whenever_instead_then_keeps_the_payload_return_bare() {
    assert_fence_row("R34", R34_FACE, &["Sorcery"], "A0: CDT{WheneverEvent}[Draw, Draw?ConditionInstead, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R35 — fence (face `p2:q4t next end instead then`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r35_p2_q4t_next_end_instead_then_keeps_the_payload_return_bare() {
    assert_fence_row("R35", R35_FACE, &["Sorcery"], "A0: CDT{AtNextPhase}[Draw, Draw?ConditionInstead, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R36 — fence (face `p3:k10 until cantcast sac then`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r36_p3_k10_until_cantcast_sac_then_keeps_the_payload_return_bare() {
    assert_fence_row("R36", R36_FACE, &["Sorcery"], "A0: CDT{AtNextPhase}[AddRestriction, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R37 — fence (face `p3:m8 cantcast until whenever sac`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r37_p3_m8_cantcast_until_whenever_sac_keeps_the_payload_return_bare() {
    assert_fence_row("R37", R37_FACE, &["Sorcery"], "A0: CDT{WheneverEvent}[AddRestriction, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R38 — fence (face `p3:m9 cantcast ltb sac`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r38_p3_m9_cantcast_ltb_sac_keeps_the_payload_return_bare() {
    assert_fence_row("R38", R38_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{AtNextPhase}[AddRestriction, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R39 — fence (face `r3:a08 foreach pump then`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r39_r3_a08_foreach_pump_then_keeps_the_payload_return_bare() {
    assert_fence_row("R39", R39_FACE, &["Sorcery"], "A0: CDT{AtNextPhase}[Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R40 — fence (face `r3:a15 until pump dur gain comma`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r40_r3_a15_until_pump_dur_gain_comma_keeps_the_payload_return_bare() {
    assert_fence_row("R40", R40_FACE, &["Sorcery"], "A0: CDT{AtNextPhase}[Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R41 — fence (face `r3:b07 nextend foreach pumpall`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r41_r3_b07_nextend_foreach_pumpall_keeps_the_payload_return_bare() {
    assert_fence_row("R41", R41_FACE, &["Sorcery"], "A0: CDT{AtNextPhase}[PumpAll, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R42 — fence (face `r3:b01 whenever foreach pump`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r42_r3_b01_whenever_foreach_pump_keeps_the_payload_return_bare() {
    assert_fence_row("R42", R42_FACE, &["Sorcery"], "A0: CDT{WheneverEvent}[Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R43 — fence (face `r3:b02 ltb prefix foreach pump`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r43_r3_b02_ltb_prefix_foreach_pump_keeps_the_payload_return_bare() {
    assert_fence_row("R43", R43_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{AtNextPhase}[Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R44 — fence (face `r3:b06 nextattack foreach pump`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r44_r3_b06_nextattack_foreach_pump_keeps_the_payload_return_bare() {
    assert_fence_row("R44", R44_FACE, &["Sorcery"], "A0: CDT{WhenNextEvent}[Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R45 — fence (face `r3:b09 inline foreach pump`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r45_r3_b09_inline_foreach_pump_keeps_the_payload_return_bare() {
    assert_fence_row("R45", R45_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: SetTapState, CDT{WhenDies}[Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,ParentTarget}]", 0);
}

/// R46 — fence (face `r3:b11 nextcast foreach pump`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r46_r3_b11_nextcast_foreach_pump_keeps_the_payload_return_bare() {
    assert_fence_row("R46", R46_FACE, &["Sorcery"], "A0: CDT{WhenNextEvent}[Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R47 — fence (face `r3:b12 searchthisway foreach pump`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r47_r3_b12_searchthisway_foreach_pump_keeps_the_payload_return_bare() {
    assert_fence_row("R47", R47_FACE, &["Sorcery"], "A0: SearchLibrary, ChangeZone{Library,Hand,Any}, Shuffle, CDT{WhenNextEvent}[Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R48 — fence (face `r3:b13 reloc mint foreach pump`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r48_r3_b13_reloc_mint_foreach_pump_keeps_the_payload_return_bare() {
    assert_fence_row("R48", R48_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R49 — fence (face `r3:u9 ifyoudo pump`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r49_r3_u9_ifyoudo_pump_keeps_the_payload_return_bare() {
    assert_fence_row("R49", R49_FACE, &["Sorcery"], "A0: CDT{AtNextPhase}[Sacrifice, Pump?EffectOutcome, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R50 — keep (face `r3:o2 suffix if otherwise return`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: a conditional instruction (`QuantityCheck`) whose `else_ability` holds the return. "Otherwise" resolves in place of that instruction (CR 608.2c). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r50_r3_o2_suffix_if_otherwise_return_binds_bare() {
    assert_keep_row("R50", R50_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[GainLife]?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "", false);
}

/// R51 — keep (face `r3:o10 earlier cond reloc otherwise`): START's bare sentinel is the binding. Why: a conditional instruction whose `else_ability` holds the return, with nothing before the conditional head (CR 608.2c).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r51_r3_o10_earlier_cond_reloc_otherwise_binds_bare() {
    assert_keep_row("R51", R51_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: GainLife?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)}, CDT{AtNextPhase}[Token, Draw]), CDT{AtNextPhase}[Token, Draw]", 0, "", false);
}

/// R52 — keep (face `r3:o12 reloc fallback otherwise`): the return binds the source's linked exile (`ExiledBySource`, the printed-host binding) on the resolution line. `Fallback` (`Unimpl=1`): the return follows the sacrifice on the tree as parsed and binds the source's linked exile (`ExiledBySource`, the printed-host binding). The face stays unsupported.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r52_r3_o12_reloc_fallback_otherwise_binds_exiled_by_source() {
    assert_keep_row("R52", R52_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, Draw], Unimpl(otherwise), ChangeZoneAll{Exile,Battlefield,ExiledBySource}", 1, "", false);
}

/// R53 — keep (face `r3:o13 suffix instead then`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: an "instead" condition (`ConditionInstead` / `TargetHasKeywordInstead`), which replaces an earlier instruction (CR 608.2c). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r53_r3_o13_suffix_instead_then_binds_bare() {
    assert_keep_row("R53", R53_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, Draw?ConditionInstead, else:(CDT{AtNextPhase}[GainLife]), ChangeZone{None,Battlefield,TrackedSet(0)}", 0, "", false);
}

/// R54 — keep (face `r3:o16 suffix cond otherwise then`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: a conditional instruction (`QuantityCheck`) whose `else_ability` holds the return. "Otherwise" resolves in place of that instruction (CR 608.2c). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r54_r3_o16_suffix_cond_otherwise_then_binds_bare() {
    assert_keep_row("R54", R54_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[GainLife]?QuantityCheck, else:(Draw, ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "", false);
}

/// R55 — keep (face `r3:o21 sacrifice cond reloc otherwise`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: a conditional instruction (`QuantityCheck`) whose `else_ability` holds the return. "Otherwise" resolves in place of that instruction (CR 608.2c). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r55_r3_o21_sacrifice_cond_reloc_otherwise_binds_bare() {
    assert_keep_row("R55", R55_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, GainLife?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)}, CDT{AtNextPhase}[Token, Draw]), CDT{AtNextPhase}[Token, Draw]", 0, "", false);
}

/// R56 — keep (face `r3:o22 outside cond reloc pushdown otherwise`): START's bare sentinel is the binding. Why: a conditional instruction whose `else_ability` holds the return, with nothing before the conditional head (CR 608.2c).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r56_r3_o22_outside_cond_reloc_pushdown_otherwise_binds_bare() {
    assert_keep_row("R56", R56_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: GainLife?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)}, CDT{AtNextPhase}[Token, GenericEffect]), CDT{AtNextPhase}[Token, GenericEffect]", 0, "", false);
}

/// R57 — keep (face `r3:o24 outside opponentmay reloc noonedoes`): START's bare sentinel is the binding. Why: "Any opponent may" (`optional` + `optional_for`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r57_r3_o24_outside_opponentmay_reloc_noonedoes_binds_bare() {
    assert_keep_row("R57", R57_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Draw, ChangeZone{None,Battlefield,TrackedSet(0)}?Not, CDT{AtNextPhase}[Token, Draw]", 0, "optional@0:Draw,optional_for@0:Draw", false);
}

/// R58 — keep (face `r3:o26 suffix keywordoverride`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: an "instead" condition (`ConditionInstead` / `TargetHasKeywordInstead`), which replaces an earlier instruction (CR 608.2c). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r58_r3_o26_suffix_keywordoverride_binds_bare() {
    assert_keep_row("R58", R58_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[SetTapState], ChangeZone{None,Battlefield,TrackedSet(0)}?TargetHasKeywordInstead", 0, "", false);
}

/// R59 — fence (face `r3:o3 suffix then otherwise`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r59_r3_o3_suffix_then_otherwise_keeps_the_payload_return_bare() {
    assert_fence_row("R59", R59_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[GainLife, Draw?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})]", 0);
}

/// R60 — fence (face `r3:o11 reloc cond otherwise`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r60_r3_o11_reloc_cond_otherwise_keeps_the_payload_return_bare() {
    assert_fence_row("R60", R60_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, Draw?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})]", 0);
}

/// R61 — fence (face `r3:o15 reloc damage instead then`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r61_r3_o15_reloc_damage_instead_then_keeps_the_payload_return_bare() {
    assert_fence_row("R61", R61_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, DealDamage, DealDamage?ConditionInstead, ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R62 — fence (face `r3:o23 reloc opponentmay noonedoes`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r62_r3_o23_reloc_opponentmay_noonedoes_keeps_the_payload_return_bare() {
    assert_fence_row("R62", R62_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, Draw, ChangeZone{None,Battlefield,TrackedSet(0)}?Not]", 0);
}

/// R63 — fence (face `r3:o25 reloc keywordoverride`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r63_r3_o25_reloc_keywordoverride_keeps_the_payload_return_bare() {
    assert_fence_row("R63", R63_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, SetTapState, ChangeZone{None,Battlefield,TrackedSet(0)}?TargetHasKeywordInstead]", 0);
}

/// R64 — fence (face `r4:q1a eta reloc otherwise earliercond`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r64_r4_q1a_eta_reloc_otherwise_earliercond_keeps_the_payload_return_bare() {
    assert_fence_row("R64", R64_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: GainLife?QuantityCheck, CDT{AtNextPhase}[Token, Token?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})]", 0);
}

/// R65 — fence (face `r4:q1b eta reloc otherwise`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r65_r4_q1b_eta_reloc_otherwise_keeps_the_payload_return_bare() {
    assert_fence_row("R65", R65_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, Token?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})]", 0);
}

/// R66 — fence (face `r4:q2b reloc dd suffix dd2 instead`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
#[test]
fn r66_r4_q2b_reloc_dd_suffix_dd2_instead_keeps_the_payload_return_bare() {
    assert_fence_row("R66", R66_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, DealDamage, DealDamage?ConditionInstead, else:(CDT{AtNextPhaseForPlayer}[DealDamage]), ChangeZone{None,Battlefield,TrackedSet(0)}]", 0);
}

/// R67 — keep (face `r4:q1c eta outside otherwise earlierreloccond`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: a conditional instruction (`QuantityCheck`) whose `else_ability` holds the return. "Otherwise" resolves in place of that instruction (CR 608.2c). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r67_r4_q1c_eta_outside_otherwise_earlierreloccond_binds_bare() {
    assert_keep_row("R67", R67_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, Draw?QuantityCheck, else:(GainLife)], Token?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "", false);
}

/// R68 — keep (face `r4:u1a reloc counter repeat otherwise`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: a conditional instruction (`QuantityCheck`) whose `else_ability` holds the return. "Otherwise" resolves in place of that instruction (CR 608.2c). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r68_r4_u1a_reloc_counter_repeat_otherwise_binds_bare() {
    assert_keep_row("R68", R68_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, CDT{AtNextPhase}[Token, PutCounter?QuantityCheck], PutCounter?QuantityCheck, PutCounter?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "", false);
}

/// R69 — fence (face `r4:ph2b ltb whenever token cleanup otherwise`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
///
/// The Phase 2 delayed-trigger nest's validating card is Dalkovan Encampment:
/// "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior creature tokens that are tapped and attacking. Sacrifice them at the beginning of the next end step."
#[test]
fn r69_r4_ph2b_ltb_whenever_token_cleanup_otherwise_keeps_the_payload_return_bare() {
    assert_fence_row("R69", R69_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{WheneverEvent}[Token, CDT{AtNextPhase}[Sacrifice]?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})]", 0);
}

/// R70 — fence (face `z:z3 cleanup comma reveal cond otherwise`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
///
/// The Phase 2 delayed-trigger nest's validating card is Dalkovan Encampment:
/// "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior creature tokens that are tapped and attacking. Sacrifice them at the beginning of the next end step."
#[test]
fn r70_z_z3_cleanup_comma_reveal_cond_otherwise_keeps_the_payload_return_bare() {
    assert_fence_row("R70", R70_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{WheneverEvent}[Token, CDT{AtNextPhase}[Sacrifice], RevealTop?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})]", 0);
}

/// R71 — fence (face `z:z4 cleanup comma flip win`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
///
/// The Phase 2 delayed-trigger nest's validating card is Dalkovan Encampment:
/// "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior creature tokens that are tapped and attacking. Sacrifice them at the beginning of the next end step."
#[test]
fn r71_z_z4_cleanup_comma_flip_win_keeps_the_payload_return_bare() {
    assert_fence_row("R71", R71_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{WheneverEvent}[Token, CDT{AtNextPhase}[Sacrifice], FlipCoins(win:ChangeZone{None,Battlefield,TrackedSet(0)})]", 0);
}

/// R72 — fence (face `z:z5 cleanup comma flip winlose`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
///
/// The Phase 2 delayed-trigger nest's validating card is Dalkovan Encampment:
/// "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior creature tokens that are tapped and attacking. Sacrifice them at the beginning of the next end step."
#[test]
fn r72_z_z5_cleanup_comma_flip_winlose_keeps_the_payload_return_bare() {
    assert_fence_row("R72", R72_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{WheneverEvent}[Token, CDT{AtNextPhase}[Sacrifice], FlipCoins(win:ChangeZone{None,Battlefield,TrackedSet(0)} / los:Draw)]", 0);
}

/// R73 — fence (face `z:z8 cleanup comma reveal3 cond otherwise`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
///
/// The Phase 2 delayed-trigger nest's validating card is Dalkovan Encampment:
/// "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior creature tokens that are tapped and attacking. Sacrifice them at the beginning of the next end step."
#[test]
fn r73_z_z8_cleanup_comma_reveal3_cond_otherwise_keeps_the_payload_return_bare() {
    assert_fence_row("R73", R73_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{WheneverEvent}[Token, CDT{AtNextPhase}[Sacrifice], RevealTop?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})]", 0);
}

/// R74 — fence (face `z:z2 cleanup comma draw cond otherwise`): a delayed-trigger payload keeps START's bare sentinel. CR 603.7c: the runtime binds every `TrackedSet(0)` sentinel in a delayed-trigger payload when the trigger is created, so the line walk never enters the payload edge and the sweep clears its mark.
///
/// No printed card exhibits this shape. The nearest printed card is Sudden Disappearance, whose creating chain has an exile producer (row R6a):
/// "Exile all nonland permanents target player controls. Return the exiled cards to the battlefield under their owner's control at the beginning of the next end step."
///
/// The Phase 2 delayed-trigger nest's validating card is Dalkovan Encampment:
/// "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior creature tokens that are tapped and attacking. Sacrifice them at the beginning of the next end step."
#[test]
fn r74_z_z2_cleanup_comma_draw_cond_otherwise_keeps_the_payload_return_bare() {
    assert_fence_row("R74", R74_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: CDT{WheneverEvent}[Token, CDT{AtNextPhase}[Sacrifice], Draw?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})]", 0);
}

/// R75 — keep (face `p2:q6 top instead single`): START's bare sentinel is the binding. Why: an "instead" condition (`ConditionInstead`), which replaces an earlier instruction (CR 608.2c); R80 also follows an exile.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r75_p2_q6_top_instead_single_binds_bare() {
    assert_keep_row(
        "R75",
        R75_FACE,
        &["Sorcery"],
        "A0: Draw, ChangeZone{None,Battlefield,TrackedSet(0)}?ConditionInstead",
        0,
        "",
        false,
    );
}

/// R76 — keep (face `c:c5 tap target and return singular`): the return binds the source's linked exile (`ExiledBySource`, the printed-host binding) on the resolution line. A straight line: "Tap target creature and return the exiled card" follows the non-exile sacrifice, so the one-card return becomes a set-wide `ChangeZoneAll` from exile over `ExiledBySource` (CR 607.2a + CR 607.3).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r76_c_c5_tap_target_and_return_singular_binds_exiled_by_source() {
    assert_keep_row("R76", R76_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, SetTapState, ChangeZoneAll{Exile,Battlefield,ExiledBySource}", 0, "", false);
}

/// R77 — keep (face `e:e1 draw if otherwise`): START's bare sentinel is the binding. Why: a conditional instruction whose `else_ability` holds the return, with nothing before the conditional head (CR 608.2c).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r77_e_e1_draw_if_otherwise_binds_bare() {
    assert_keep_row(
        "R77",
        R77_FACE,
        &["Sorcery"],
        "A0: Draw?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})",
        0,
        "",
        false,
    );
}

/// R78 — keep (face `e:e4 sac exile if otherwise`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: a conditional instruction (`QuantityCheck`) whose `else_ability` holds the return. "Otherwise" resolves in place of that instruction (CR 608.2c). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r78_e_e4_sac_exile_if_otherwise_binds_bare() {
    assert_keep_row("R78", R78_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}?QuantityCheck, else:(ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "", false);
}

/// R80 — keep (face `e:i3 exile instead sentence`): START's bare sentinel is the binding. Why: an "instead" condition (`ConditionInstead`), which replaces an earlier instruction (CR 608.2c); R80 also follows an exile.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r80_e_i3_exile_instead_sentence_binds_bare() {
    assert_keep_row("R80", R80_FACE, &["Sorcery"], "A0: ChangeZone{None,Exile,Typed}, Draw?ConditionInstead, ChangeZone{None,Battlefield,TrackedSet(0)}", 0, "", false);
}

/// R83 — keep (face `v:v6 sac opp may exile no one does`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: "Any opponent may" (`optional` + `optional_for`) is a decision (CR 118.12). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r83_v_v6_sac_opp_may_exile_no_one_does_binds_bare() {
    assert_keep_row("R83", R83_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, ChangeZone{None,Battlefield,TrackedSet(0)}?Not", 0, "optional@1:ChangeZone,optional_for@1:ChangeZone", false);
}

/// R84 — keep (face `v:v5 opp may draw no one does`): START's bare sentinel is the binding. Why: "Any opponent may" (`optional` + `optional_for`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r84_v_v5_opp_may_draw_no_one_does_binds_bare() {
    assert_keep_row("R84", R84_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Draw, ChangeZone{None,Battlefield,TrackedSet(0)}?Not", 0, "optional@0:Draw,optional_for@0:Draw", false);
}

/// R85 — keep (face `v:v1 sac may exile if you dont`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: an instruction's own "may" (`optional`) is a decision (CR 118.12). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r85_v_v1_sac_may_exile_if_you_dont_binds_bare() {
    assert_keep_row("R85", R85_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, ChangeZone{None,Battlefield,TrackedSet(0)}?Not", 0, "optional@1:ChangeZone", false);
}

/// R86 — keep (face `v:v11 sac may exile if do draw otherwise`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: an instruction's own "may" (`optional`) is a decision (CR 118.12). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r86_v_v11_sac_may_exile_if_do_draw_otherwise_binds_bare() {
    assert_keep_row("R86", R86_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, Draw?EffectOutcome, else:(ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "optional@1:ChangeZone", false);
}

/// R87 — keep (face `v:v10 may sac if do draw otherwise sorcery`): START's bare sentinel is the binding. Why: an instruction's own "may" (`optional`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r87_v_v10_may_sac_if_do_draw_otherwise_sorcery_binds_bare() {
    assert_keep_row(
        "R87",
        R87_FACE,
        &["Sorcery"],
        "A0: Sacrifice, Draw?EffectOutcome, else:(ChangeZone{None,Battlefield,TrackedSet(0)})",
        0,
        "optional@0:Sacrifice",
        false,
    );
}

/// R88 — keep (face `v:v12 sac may exile if dont sentence return`): START's bare sentinel is the binding. Why: an instruction's own "may" (`optional`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r88_v_v12_sac_may_exile_if_dont_sentence_return_binds_bare() {
    assert_keep_row("R88", R88_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, Draw?Not, ChangeZone{None,Battlefield,TrackedSet(0)}", 0, "optional@1:ChangeZone", false);
}

/// R89 — keep (face `w:w1 sac may exile dont draw and gain return`): START's bare sentinel is the binding. Why: an instruction's own "may" (`optional`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r89_w_w1_sac_may_exile_dont_draw_and_gain_return_binds_bare() {
    assert_keep_row("R89", R89_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, Draw?Not, GainLife, ChangeZone{None,Battlefield,TrackedSet(0)}", 0, "optional@1:ChangeZone", false);
}

/// R93 — keep (face `w:w26 draw unless pays they do return sorcery`): START's bare sentinel is the binding. Why: the head's unless cost (`unless_pay`; CR 118.12a).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r93_w_w26_draw_unless_pays_they_do_return_sorcery_binds_bare() {
    assert_keep_row(
        "R93",
        R93_FACE,
        &["Sorcery"],
        "A0: Draw, ChangeZone{None,Battlefield,TrackedSet(0)}?EffectOutcome",
        0,
        "unless_pay@0:Draw",
        false,
    );
}

/// R94 — keep (face `w:w27 draw unless pays they dont return sorcery`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: the head's unless cost (`unless_pay`; CR 118.12a). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r94_w_w27_draw_unless_pays_they_dont_return_sorcery_binds_bare() {
    assert_keep_row(
        "R94",
        R94_FACE,
        &["Sorcery"],
        "A0: Draw, ChangeZone{None,Battlefield,TrackedSet(0)}?Not",
        0,
        "unless_pay@0:Draw",
        false,
    );
}

/// R95 — keep (face `w:w25 sac each opp may exile dont return`): START's bare sentinel is the binding. Why: a "may" offered per player (`optional` + `player_scope`) is a decision made on each pass (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r95_w_w25_sac_each_opp_may_exile_dont_return_binds_bare() {
    assert_keep_row("R95", R95_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, ChangeZone{None,Battlefield,TrackedSet(0)}?Not", 0, "optional@1:ChangeZone,player_scope@1:ChangeZone", false);
}

/// R96 — keep (face `w:w6 sac opp may exile player does draw no one return`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: "Any opponent may" (`optional` + `optional_for`) is a decision (CR 118.12). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r96_w_w6_sac_opp_may_exile_player_does_draw_no_one_r_binds_bare() {
    assert_keep_row("R96", R96_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, Draw?EffectOutcome, else:(ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "optional@1:ChangeZone,optional_for@1:ChangeZone", false);
}

/// R97 — keep (face `w:w18 opp may draw player does gain no one return`): START's bare sentinel is the binding. Why: "Any opponent may" (`optional` + `optional_for`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r97_w_w18_opp_may_draw_player_does_gain_no_one_retur_binds_bare() {
    assert_keep_row("R97", R97_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Draw, GainLife?EffectOutcome, else:(ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "optional@0:Draw,optional_for@0:Draw", false);
}

/// R98 — keep (face `w:w17 may pay dont return sorcery`): START's bare sentinel is the binding. Why: an instruction's own "may" (`optional`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r98_w_w17_may_pay_dont_return_sorcery_binds_bare() {
    assert_keep_row(
        "R98",
        R98_FACE,
        &["Sorcery"],
        "A0: PayCost, ChangeZone{None,Battlefield,TrackedSet(0)}?Not",
        0,
        "optional@0:PayCost",
        false,
    );
}

/// R99 — keep (face `y:y1`): START's bare sentinel is the binding. Why: an instruction's own "may" (`optional`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r99_y_y1_binds_bare() {
    assert_keep_row("R99", R99_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, Draw?EffectOutcome, ChangeZone{None,Battlefield,TrackedSet(0)}?Not", 0, "optional@1:ChangeZone,optional@2:Draw", false);
}

/// R100 — keep (face `y:y2`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: the head's "may", and then the rider's own "may" (CR 118.12). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r100_y_y2_binds_bare() {
    assert_keep_row(
        "R100",
        R100_FACE,
        &["Sorcery"],
        "A0: Draw, Sacrifice?EffectOutcome, ChangeZone{None,Battlefield,TrackedSet(0)}?Not",
        0,
        "optional@0:Draw,optional@1:Sacrifice",
        false,
    );
}

/// R101 — keep (face `y:y3`): START's bare sentinel is the binding. Why: an instruction's own "may" (`optional`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r101_y_y3_binds_bare() {
    assert_keep_row("R101", R101_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, Draw?EffectOutcome, GainLife?EffectOutcome, else:(ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "optional@1:ChangeZone,optional@2:Draw", false);
}

/// R102 — keep (face `y:y4`): START's bare sentinel is the binding. Why: a "may" offered per player (`optional` + `player_scope`) is a decision made on each pass (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r102_y_y4_binds_bare() {
    assert_keep_row("R102", R102_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, Draw?EffectOutcome, ChangeZone{None,Battlefield,TrackedSet(0)}?Not", 0, "optional@1:ChangeZone,optional@2:Draw,player_scope@2:Draw", false);
}

/// R103 — keep (face `y:y5`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: a "may" offered per player (`optional` + `player_scope`) is a decision made on each pass (CR 118.12). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r103_y_y5_binds_bare() {
    assert_keep_row(
        "R103",
        R103_FACE,
        &["Sorcery"],
        "A0: Sacrifice, Sacrifice?EffectOutcome, ChangeZone{None,Battlefield,TrackedSet(0)}?Not",
        0,
        "optional@0:Sacrifice,optional@1:Sacrifice,player_scope@1:Sacrifice",
        false,
    );
}

/// R105 — keep (face `y:y7`): START's bare sentinel is the binding. Why: the return's own "If you can't" (`Not(ZoneChangedThisWay)`), which is not the admitted gate (CR 118.12's "can't").
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r105_y_y7_binds_bare() {
    assert_keep_row("R105", R105_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}?Not", 0, "", true);
}

/// R106 — keep (face `y:y8`): START's bare sentinel is the binding. Why: the return's own "If you can't" (`Not(ZoneChangedThisWay)`), which is not the admitted gate (CR 118.12's "can't").
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r106_y_y8_binds_bare() {
    assert_keep_row(
        "R106",
        R106_FACE,
        &["Sorcery"],
        "A0: Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}?Not",
        0,
        "",
        true,
    );
}

/// R107 — keep (face `w:w13`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: the return's own "If you can't" (`Not(ZoneChangedThisWay)`), which is not the admitted gate (CR 118.12's "can't"). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r107_w_w13_binds_bare() {
    assert_keep_row("R107", R107_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, ChangeZone{None,Battlefield,TrackedSet(0)}?Not", 0, "", true);
}

/// R109 — keep (face `u:u5`): START's bare sentinel is the binding. Why: an instruction's own "may" (`optional`) is a decision (CR 118.12).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r109_u_u5_binds_bare() {
    assert_keep_row("R109", R109_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZone{None,Exile,Typed}, Sacrifice?EffectOutcome, ChangeZone{None,Battlefield,TrackedSet(0)}?Not", 0, "optional@1:ChangeZone", true);
}

/// R110 — keep (face `u:u11`): START's bare sentinel is the binding. Why: the head's unless cost (`unless_pay`; CR 118.12a).
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r110_u_u11_binds_bare() {
    assert_keep_row(
        "R110",
        R110_FACE,
        &["Sorcery"],
        "A0: Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}?Not",
        0,
        "unless_pay@0:Sacrifice",
        true,
    );
}

/// R112 — keep (face `u:u9`): the return binds the source's linked exile (`ExiledBySource`, the printed-host binding) on the resolution line. The admitted gate (CR 118.12, CR 608.2c): the return resolves only after the mandatory sacrifice resolved, and the sacrifice is not an exile, so the return binds the source's linked exile (`ExiledBySource`, the printed-host binding). It is red when the gate is not admitted.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
///
/// The admitted gate's printed card is The Spot, Living Portal:
/// "When The Spot dies, put him on the bottom of his owner's library. If you do, return the exiled cards to their owners' hands."
#[test]
fn r112_u_u9_binds_exiled_by_source() {
    assert_keep_row("R112", R112_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, ChangeZoneAll{Exile,Battlefield,ExiledBySource}?EffectOutcome", 0, "", false);
}

/// R113 — keep (face `e2:e9 flip win return`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: the `FlipCoins` win branch, a branch edge (CR 705.1). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r113_e2_e9_flip_win_return_binds_bare() {
    assert_keep_row("R113", R113_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: FlipCoins(win:ChangeZone{None,Battlefield,TrackedSet(0)})", 0, "", false);
}

/// R114 — keep (face `p2:q9 third chunk leading if single`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: the return's own condition, "If you control a Dragon" (`QuantityCheck`; CR 608.2c). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r114_p2_q9_third_chunk_leading_if_single_binds_bare() {
    assert_keep_row(
        "R114",
        R114_FACE,
        &["Sorcery"],
        "A0: Draw, GainLife, ChangeZone{None,Battlefield,TrackedSet(0)}?QuantityCheck",
        0,
        "",
        false,
    );
}

/// R115 — keep (face `w:w7`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: an instruction's own "may" (`optional`) is a decision (CR 118.12). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r115_w_w7_binds_bare() {
    assert_keep_row(
        "R115",
        R115_FACE,
        &["Sorcery"],
        "A0: Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}?EffectOutcome",
        0,
        "optional@0:Sacrifice",
        false,
    );
}

/// R116 — keep (face `p2:x5 modal in prefix`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: the return is in a mode (`mode_abilities`). No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r116_p2_x5_modal_in_prefix_binds_bare() {
    assert_keep_row("R116", R116_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: GenericEffect, mode0:(Draw, ChangeZone{None,Battlefield,TrackedSet(0)}), mode1:(GainLife)", 0, "", false);
}

/// R117 — keep (face `r3:b14 top foreach pump`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: "For each opponent, ..." (`repeat_for` on the `Pump`) repeats the instruction. No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r117_r3_b14_top_foreach_pump_binds_bare() {
    assert_keep_row(
        "R117",
        R117_FACE,
        &["Sorcery"],
        "A0: Sacrifice, Pump, GenericEffect, Sacrifice, ChangeZone{None,Battlefield,TrackedSet(0)}",
        0,
        "repeat_for@1:Pump",
        false,
    );
}

/// R118 — keep (face `e:h19 then each opp loses`): START's bare sentinel is the binding.
/// Keeps START's binding here: the return is off the resolution line (`stays_on_resolution_line`). Why: "Each opponent loses 2 life" (`player_scope` on the `LoseLife`) is an instruction applied per player. No printed card has this shape, and the unfiltered binding is the safe fallback.
///
/// No printed card exhibits this shape. The nearest printed card is Day of the Dragons, whose own-chain return follows a non-exile sacrifice (R1):
/// "When this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control."
#[test]
fn r118_e_h19_then_each_opp_loses_binds_bare() {
    assert_keep_row("R118", R118_FACE, &["Enchantment"], "T0: ChangeZoneAll{None,Exile,Typed} | T1: Sacrifice, LoseLife, ChangeZone{None,Battlefield,TrackedSet(0)}", 0, "player_scope@1:LoseLife", false);
}
