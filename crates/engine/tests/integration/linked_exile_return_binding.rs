//! CR 607.2a + CR 406.6: "the exiled card" / "the exiled cards" in a return
//! whose own resolution chain has prior tracked-set publishers, none of them an
//! exile instruction, refers only to cards put into exile by the linked exile
//! instruction — never to the chain's own sacrificed or library-put members.
//! The parser binds the bare chain sentinel to the `Exiled`-caused subset
//! (`TrackedSetFiltered { id: 0, filter: Any, caused_by: Some(Exiled) }`).
//!
//! Runtime rows (R1, R9) drive the real cast pipeline through `GameScenario`
//! with cards built from live-parsed Oracle text (never the committed fixture),
//! so a START run measures START's parser. Shape rows (R2–R6, R8) parse with
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

/// Builds R1's board (optionally with Rest in Peace under P1), casts Day of
/// the Dragons with no creatures, then Dragon Hatchling, then Disenchant on
/// Day. Returns `(day, hatchling, events of the Disenchant drain, runner)`.
fn day_of_the_dragons_with_hatchling(
    rest_in_peace: bool,
) -> (
    ObjectId,
    ObjectId,
    Vec<GameEvent>,
    engine::game::scenario::GameRunner,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if rest_in_peace {
        scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    }
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
    // Reach guard (1): Day's ETB resolved over an empty board — nothing exiled,
    // zero Dragon tokens.
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
        dragon_tokens, 0,
        "Day's ETB exiled nothing, so it creates 0 tokens"
    );

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
    (day, hatchling, events, runner)
}

/// R1 — CR 607.2a + CR 406.6 + CR 701.21a: Day of the Dragons' LTB sacrifices
/// all Dragons, then returns "the exiled cards" — the cards its ETB exiled.
/// A sacrificed nontoken Dragon card is put into the graveyard by the sacrifice
/// (CR 701.21a), never into exile by the linked instruction, so it stays in the
/// graveyard. (This row asserts nothing about an ETB-exiled card; there is none.)
#[test]
fn day_of_the_dragons_ltb_does_not_return_a_sacrificed_dragon_card() {
    let (day, hatchling, events, runner) = day_of_the_dragons_with_hatchling(false);
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

/// R9 — CR 607.2a + CR 614.6 + CR 701.21a: under Rest in Peace the sacrifice
/// puts Dragon Hatchling into exile instead of the graveyard, but the card was
/// still sacrificed — never put into exile by Day's ETB instruction — so the
/// return leaves it in exile. Only the `caused_by: Exiled` binding decides this
/// row: the return scans exile and reaches the card.
#[test]
fn day_of_the_dragons_ltb_does_not_return_a_sacrificed_card_redirected_to_exile() {
    let (day, hatchling, events, runner) = day_of_the_dragons_with_hatchling(true);
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
    assert!(state.stack.is_empty(), "the stack drained");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "fully resolved, no stray prompt: {:?}",
        state.waiting_for
    );

    assert_ne!(
        state.objects[&hatchling].zone,
        Zone::Battlefield,
        "a sacrificed card redirected to exile is not one of 'the exiled cards' (CR 607.2a)"
    );
    assert_eq!(state.objects[&hatchling].zone, Zone::Exile);
}

/// R2 — Day of the Dragons' plural return lowers to a set-wide
/// `ChangeZoneAll { origin: Exile }` over the `Exiled`-caused sentinel, never the
/// single-pick `ChangeZone` route.
#[test]
fn day_of_the_dragons_return_binds_exiled_caused_set_wide() {
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
            assert_eq!(*target, exiled_caused_sentinel());
            assert_eq!(*enters_under, Some(ControllerRef::You));
        }
        other => panic!("Day's return is a set-wide ChangeZoneAll, not {other:?}"),
    }
}

/// R3 — the quoted granted trigger is parsed with a fresh context: the parent's
/// "exile it" cannot reach it, so the granted chain's only prior clause is its
/// own sacrifice and "the exiled card" binds the `Exiled`-caused sentinel.
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

/// R4 — The Spot's plural hand return stays a `Bounce` (no set-wide promotion
/// for a hand return with no origin) and binds the `Exiled`-caused sentinel.
#[test]
fn the_spot_dies_return_binds_exiled_caused_sentinel() {
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
        Effect::Bounce {
            target,
            destination,
            ..
        } => {
            assert_eq!(*target, exiled_caused_sentinel());
            assert_eq!(*destination, None);
        }
        other => panic!("The Spot's return is a Bounce, not {other:?}"),
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
/// CR 603.7c: a delayed trigger's binding is fixed at creation and the later
/// obligations (incarnation pins, consumed-set release) key on the bare
/// `TrackedSet` shape, so the cause-bound return rewrite stays inside the
/// return's own resolution chain and never crosses into the delayed body.
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
