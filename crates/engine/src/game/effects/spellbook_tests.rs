//! Tests for the Alchemy spellbook draft (`Effect::DraftFromSpellbook`).
//! Declared from `effects/mod.rs` so `spellbook.rs` stays implementation-only.

use super::resolve_ability_chain;
use super::spellbook::{complete_draft, resolve};
use crate::game::zones::create_object;
use crate::parser::oracle_effect::parse_effect;
use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::CardId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn draft_ability(
    source: crate::types::identifiers::ObjectId,
    destination: Zone,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DraftFromSpellbook {
            destination,
            tapped: false,
            random: false,
        },
        Vec::new(),
        source,
        PlayerId(0),
    )
}

/// A source object carrying a spellbook list.
fn source_with_spellbook(
    state: &mut GameState,
    names: &[&str],
) -> crate::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(1),
        PlayerId(0),
        "Adaptive Armorer".to_string(),
        Zone::Battlefield,
    );
    state.objects.get_mut(&id).unwrap().spellbook = names.iter().map(|s| s.to_string()).collect();
    id
}

#[test]
fn resolve_raises_choice_from_the_sources_spellbook() {
    // The resolver reads the list off the source object and pauses for a choice.
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &["Fireshrieker", "Lion Sash", "Fishing Pole"]);

    let mut events = Vec::new();
    resolve(&mut state, &draft_ability(source, Zone::Hand), &mut events).expect("resolves");

    match &state.waiting_for {
        WaitingFor::SpellbookDraft {
            player,
            options,
            destination,
            ..
        } => {
            assert_eq!(*player, PlayerId(0));
            assert_eq!(options.len(), 3);
            assert!(options.iter().any(|o| o == "Lion Sash"));
            assert_eq!(*destination, Zone::Hand);
        }
        other => panic!("expected SpellbookDraft, got {other:?}"),
    }
}

#[test]
fn resolve_is_a_noop_when_the_source_has_no_spellbook() {
    // With no spellbook list, the draft resolves without pausing.
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &[]);

    let mut events = Vec::new();
    resolve(&mut state, &draft_ability(source, Zone::Hand), &mut events).expect("resolves");

    assert!(
        !matches!(state.waiting_for, WaitingFor::SpellbookDraft { .. }),
        "an empty spellbook must not pause on a choice"
    );
}

#[test]
fn resolve_chain_stashes_spellbook_continuation_until_choice_resolves() {
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &["Fireshrieker"]);
    create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Drawn Card".to_string(),
        Zone::Library,
    );

    let draw_tail = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        source,
        PlayerId(0),
    );
    let ability = draft_ability(source, Zone::Hand).sub_ability(draw_tail);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("resolves to choice");

    assert!(matches!(
        state.waiting_for,
        WaitingFor::SpellbookDraft { .. }
    ));
    assert!(
        state.active_ability_continuation().is_some(),
        "the Draw tail must wait until the spellbook choice is submitted"
    );
    assert_eq!(
        state.players[0].hand.len(),
        0,
        "the tail must not run before the spellbook choice resolves"
    );
}

#[test]
fn complete_draft_conjures_the_chosen_card_into_the_destination() {
    // Choosing a card from the list creates it in the destination zone (via the
    // shared conjure path).
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &["Fireshrieker", "Lion Sash"]);
    let options = vec!["Fireshrieker".to_string(), "Lion Sash".to_string()];

    let mut events = Vec::new();
    complete_draft(
        &mut state,
        PlayerId(0),
        source,
        &options,
        "Lion Sash",
        Zone::Hand,
        false,
        &mut events,
    )
    .expect("the chosen card is conjured");

    let made = state.players[0]
        .hand
        .iter()
        .filter_map(|id| state.objects.get(id))
        .any(|o| o.name == "Lion Sash");
    assert!(made, "the chosen card is created in the controller's hand");
}

#[test]
fn complete_draft_rejects_a_card_not_in_the_offered_list() {
    let mut state = GameState::new_two_player(42);
    let source = source_with_spellbook(&mut state, &["Fireshrieker"]);
    let options = vec!["Fireshrieker".to_string()];

    let mut events = Vec::new();
    let result = complete_draft(
        &mut state,
        PlayerId(0),
        source,
        &options,
        "Black Lotus",
        Zone::Hand,
        false,
        &mut events,
    );
    assert!(result.is_err(), "a card outside the spellbook is illegal");
}

#[test]
fn parser_maps_draft_clauses_to_the_right_destination() {
    // Default → hand; "put it onto the battlefield" → battlefield; "exile it" → exile.
    // Trailing periods (as cards actually print) must still match.
    assert!(matches!(
        parse_effect("draft a card from Big Spender's spellbook."),
        Effect::DraftFromSpellbook {
            destination: Zone::Hand,
            tapped: false,
            random: false,
        }
    ));
    assert!(matches!(
        parse_effect(
            "draft a card from Adaptive Armorer's spellbook and put it onto the battlefield."
        ),
        Effect::DraftFromSpellbook {
            destination: Zone::Battlefield,
            tapped: false,
            random: false,
        }
    ));
    assert!(matches!(
        parse_effect("draft a card from this creature's spellbook and exile it."),
        Effect::DraftFromSpellbook {
            destination: Zone::Exile,
            tapped: false,
            random: false,
        }
    ));
    // Production-normalized Arms Scavenger / Tibalt rider: `, then exile it`
    // is the same destination as Kayla's ` and exile it`.
    assert!(matches!(
        parse_effect("draft a card from ~'s spellbook, then exile it"),
        Effect::DraftFromSpellbook {
            destination: Zone::Exile,
            tapped: false,
            random: false,
        }
    ));
    // Single-clause `parse_effect` does not split on the possessive apostrophe;
    // the rider combinator still sees `, then exile it`.
    assert!(matches!(
        parse_effect("draft a card from this creature's spellbook, then exile it"),
        Effect::DraftFromSpellbook {
            destination: Zone::Exile,
            tapped: false,
            random: false,
        }
    ));
}

#[test]
fn parser_honours_the_tapped_rider() {
    // CR-correct battlefield state: "...onto the battlefield tapped." sets tapped.
    assert!(matches!(
        parse_effect(
            "draft a card from this creature's spellbook and put it onto the battlefield tapped."
        ),
        Effect::DraftFromSpellbook {
            destination: Zone::Battlefield,
            tapped: true,
            random: false,
        }
    ));
}

#[test]
fn parser_rejects_unmodeled_riders_as_unimplemented() {
    // "exile it face down", "twice, then …", and other unmodeled tails must NOT
    // collapse to a wrong effect — they fall through to a clean Unimplemented so
    // the coverage tooling flags them (and no clause is silently swallowed).
    for text in [
        "draft a card from this creature's spellbook and exile it face down.",
        "draft a card from this creature's spellbook twice, then put those cards onto the battlefield.",
        "draft a card from this creature's spellbook twice, then put one of those cards onto the battlefield tapped.",
    ] {
        assert!(
            !matches!(parse_effect(text), Effect::DraftFromSpellbook { .. }),
            "unmodeled spellbook rider must not parse to DraftFromSpellbook: {text:?}"
        );
    }
}

/// Runtime DRIVER + RESOLVER guard for the interactive Alchemy draft. Builds a
/// battlefield permanent with a `{T}: DraftFromSpellbook` activated ability,
/// seeds its spellbook, and drives it through the real activation pipeline via
/// the new `.spellbook_pick(..)` driver hook. Reverting the driver's
/// `SpellbookDraft` arm (or the `spellbook_pick` threading) leaves the pick
/// unanswered: the draft never completes, the card never reaches hand, and the
/// pipeline halts at `SpellbookDraft` instead of `Priority` — flipping both
/// assertions. This guards the driver/resolver, NOT the data pipeline (that
/// revert guard is the oracle_gen `build_token_source_metadata` merge test).
#[test]
fn spellbook_pick_drives_the_draft_and_conjures_the_chosen_card() {
    use crate::game::scenario::GameScenario;
    use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
    use crate::types::phase::Phase;

    let p0 = PlayerId(0);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(p0, "Alchemist", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DraftFromSpellbook {
                    destination: Zone::Hand,
                    tapped: false,
                    random: false,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();
    let mut runner = scenario.build();

    // Seed the drafting source's spellbook — the runtime data the pipeline fix
    // populates at export from AtomicCards' relatedCards.spellbook.
    let list = ["Fireshrieker", "Lion Sash", "Fishing Pole"];
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .spellbook = list.iter().map(|s| s.to_string()).collect();

    let outcome = runner
        .activate(source, 0)
        .spellbook_pick("Lion Sash")
        .resolve();

    let drafted = outcome.state().players[0]
        .hand
        .iter()
        .filter_map(|id| outcome.state().objects.get(id))
        .any(|o| o.name == "Lion Sash");
    assert!(
        drafted,
        "the driver must conjure the declared spellbook pick into P0's hand"
    );
    // Positive reach-guard (non-vacuous): the driver actually reached the
    // SpellbookDraft halt, answered it, and drove resolution back to Priority.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must return to Priority after the draft, got {:?}",
        outcome.final_waiting_for()
    );
}

/// Runtime guard for the RANDOM spellbook draft ("conjure a random card from
/// ~'s spellbook"): unlike the interactive draft it must NOT pause for a pick —
/// the engine picks from the source's spellbook and conjures immediately. A
/// single-card spellbook makes the pick deterministic regardless of the RNG.
/// Reverting the `random` resolver arm re-introduces the `SpellbookDraft` pause,
/// so resolution would halt there instead of returning to `Priority`, flipping
/// both assertions.
#[test]
fn random_spellbook_draft_conjures_without_pausing() {
    use crate::game::scenario::GameScenario;
    use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
    use crate::types::phase::Phase;

    let p0 = PlayerId(0);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(p0, "Alchemist", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DraftFromSpellbook {
                    destination: Zone::Hand,
                    tapped: false,
                    random: true,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();
    let mut runner = scenario.build();

    // Single-card spellbook -> the random pick is deterministic.
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .spellbook = vec!["Lion Sash".to_string()];

    // No `.spellbook_pick(..)`: a random draft must resolve without pausing.
    let outcome = runner.activate(source, 0).resolve();

    let drafted = outcome.state().players[0]
        .hand
        .iter()
        .filter_map(|id| outcome.state().objects.get(id))
        .any(|o| o.name == "Lion Sash");
    assert!(
        drafted,
        "a random spellbook draft must conjure a card from the list into hand"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "a random draft must not pause on SpellbookDraft; got {:?}",
        outcome.final_waiting_for()
    );
}

/// Multi-authority provenance: with two permanents carrying DIFFERENT
/// spellbooks, activating one must offer ONLY that source's list — proving the
/// offered options come from the drafting source, not any other permanent.
/// Activating without a `.spellbook_pick(..)` halts cleanly at `SpellbookDraft`
/// (the driver's no-pick break), which we inspect for the offered options.
#[test]
fn spellbook_draft_offers_only_the_activated_sources_list() {
    use crate::game::scenario::GameScenario;
    use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
    use crate::types::phase::Phase;

    let p0 = PlayerId(0);
    let draft = || {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DraftFromSpellbook {
                destination: Zone::Hand,
                tapped: false,
                random: false,
            },
        )
        .cost(AbilityCost::Tap)
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source_a = scenario
        .add_creature(p0, "Archivist A", 1, 1)
        .with_ability_definition(draft())
        .id();
    let source_b = scenario
        .add_creature(p0, "Archivist B", 1, 1)
        .with_ability_definition(draft())
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&source_a)
        .unwrap()
        .spellbook = vec!["Alpha One".to_string(), "Alpha Two".to_string()];
    runner
        .state_mut()
        .objects
        .get_mut(&source_b)
        .unwrap()
        .spellbook = vec![
        "Beta One".to_string(),
        "Beta Two".to_string(),
        "Beta Three".to_string(),
    ];

    // Activate A with NO pick → the driver halts at the draft boundary.
    let outcome = runner.activate(source_a, 0).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::SpellbookDraft {
            source_id, options, ..
        } => {
            assert_eq!(
                *source_id, source_a,
                "the draft must be sourced from the activated permanent"
            );
            assert_eq!(
                options,
                &vec!["Alpha One".to_string(), "Alpha Two".to_string()],
                "only the activated source's spellbook may be offered, not the other permanent's"
            );
        }
        other => panic!("expected halt at SpellbookDraft, got {other:?}"),
    }
}

/// Verbatim Oracle of Arms Scavenger (Scryfall). The equip cost-reduction
/// static is out of scope for these assertions.
const ARMS_SCAVENGER_ORACLE: &str = "At the beginning of your upkeep, draft a card from this creature's spellbook, then exile it. Until end of turn, you may play that card.\nEquip abilities you activate cost {1} less to activate.";

/// SHAPE: production `parse_oracle_text` of Arms Scavenger must lower the upkeep
/// execute to `DraftFromSpellbook { Exile }` plus an anaphoric play grant, not
/// `Unimplemented`. Revert the `, then exile it` rider and this execute is
/// Unimplemented again.
#[test]
fn arms_scavenger_parse_oracle_text_shape() {
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{CardPlayMode, CastingPermission, Duration};
    use crate::types::identifiers::TrackedSetId;
    use crate::types::phase::Phase;
    use crate::types::triggers::TriggerMode;

    let parsed = parse_oracle_text(ARMS_SCAVENGER_ORACLE, "Arms Scavenger", &[], &[], &[]);
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .expect("Arms Scavenger must parse an upkeep trigger");
    let execute = trigger
        .execute
        .as_deref()
        .expect("upkeep trigger must have an execute clause");
    assert!(
        matches!(
            &*execute.effect,
            Effect::DraftFromSpellbook {
                destination: Zone::Exile,
                tapped: false,
                random: false,
            }
        ),
        "upkeep execute must be DraftFromSpellbook {{ Exile }}, not Unimplemented: {:?}",
        execute.effect
    );
    let grant = execute
        .sub_ability
        .as_deref()
        .expect("play-that-card grant must chain after the draft");
    match &*grant.effect {
        Effect::GrantCastingPermission {
            permission:
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    mode,
                    ..
                },
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            ..
        } => {
            assert_eq!(
                *mode,
                CardPlayMode::Play,
                "Arms Scavenger says \"play that card\", not cast"
            );
        }
        other => panic!(
            "expected GrantCastingPermission PlayFromExile UntilEndOfTurn TrackedSet(0) Play, got {other:?}"
        ),
    }
}

/// SHAPE: Tibalt, Wicked Tormentor +1 body uses the same `, then exile it`
/// rider and grants a **cast** permission.
#[test]
fn tibalt_wicked_tormentor_plus_one_parse_shape() {
    use crate::parser::oracle_effect::parse_effect_chain;
    use crate::types::ability::{AbilityKind, CardPlayMode, CastingPermission, Duration};
    use crate::types::identifiers::TrackedSetId;

    let def = parse_effect_chain(
        "draft a card from ~'s spellbook, then exile it. Until end of turn, you may cast that card.",
        AbilityKind::Activated,
    );
    assert!(
        matches!(
            &*def.effect,
            Effect::DraftFromSpellbook {
                destination: Zone::Exile,
                tapped: false,
                random: false,
            }
        ),
        "Tibalt +1 execute must be DraftFromSpellbook {{ Exile }}, got {:?}",
        def.effect
    );
    let grant = def
        .sub_ability
        .as_deref()
        .expect("cast-that-card grant must chain after the draft");
    match &*grant.effect {
        Effect::GrantCastingPermission {
            permission:
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    mode: CardPlayMode::Cast,
                    ..
                },
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            ..
        } => {}
        other => panic!(
            "expected GrantCastingPermission PlayFromExile UntilEndOfTurn TrackedSet(0) Cast, got {other:?}"
        ),
    }
}

/// RUNTIME: verbatim Arms Scavenger Oracle on the battlefield. Beginning-of-upkeep
/// (CR 503.1a) must pause on `SpellbookDraft`, and answering it must put the
/// chosen card in exile with a `PlayFromExile` `UntilEndOfTurn` permission
/// (CR 608.2c + CR 611.2a), not in hand. Revert the rider → no draft prompt.
/// Revert the publish → card may sit in exile with `casting_permissions == []`.
#[test]
fn arms_scavenger_upkeep_drafts_into_exile_with_play_grant() {
    use crate::game::scenario::{GameScenario, P0};
    use crate::game::triggers::drain_order_triggers_with_identity;
    use crate::types::ability::{CardPlayMode, CastingPermission, Duration};
    use crate::types::actions::GameAction;
    use crate::types::phase::Phase;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.with_library_top(P0, &["Pad A", "Pad B", "Pad C", "Pad D"]);
    let source = scenario
        .add_creature_from_oracle(P0, "Arms Scavenger", 2, 2, ARMS_SCAVENGER_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .spellbook = vec![
        "Fireshrieker".to_string(),
        "Lion Sash".to_string(),
        "Fishing Pole".to_string(),
    ];

    runner.advance_to_upkeep();
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "reach-guard: the beginning-of-upkeep trigger window must have opened"
    );
    for _ in 0..16 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        if matches!(
            runner.state().waiting_for,
            WaitingFor::SpellbookDraft { .. }
        ) {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && !runner.state().stack.is_empty()
        {
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority must resolve the upkeep trigger toward the draft");
            continue;
        }
        panic!(
            "expected OrderTriggers, stacked Priority, or SpellbookDraft during Arms upkeep, got {:?} (stack={})",
            runner.state().waiting_for,
            runner.state().stack.len()
        );
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::SpellbookDraft { .. }
        ),
        "the upkeep execute must pause on SpellbookDraft, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SubmitSpellbookDraft {
            card: "Lion Sash".to_string(),
        })
        .expect("the Arms Scavenger pick must be accepted");

    let state = runner.state();
    let drafted = state
        .objects
        .values()
        .find(|o| o.name == "Lion Sash")
        .expect("the drafted card must exist");
    assert_eq!(
        drafted.zone,
        Zone::Exile,
        "CR 701.13a: the drafted card is created in exile, not moved there from hand"
    );
    let in_hand = state.players[0]
        .hand
        .iter()
        .filter_map(|id| state.objects.get(id))
        .any(|o| o.name == "Lion Sash");
    assert!(!in_hand, "the drafted card must not visit hand");
    assert!(
        drafted.casting_permissions.iter().any(|perm| matches!(
            perm,
            CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfTurn,
                mode: CardPlayMode::Play,
                ..
            }
        )),
        "CR 608.2c + CR 611.2a: until end of turn you may play that card; got {:?}",
        drafted.casting_permissions
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to Priority after the draft, got {:?}",
        state.waiting_for
    );
}

/// RUNTIME sibling: Kayla's Kindling analog (` and exile it` + cast grant).
/// The publish fix is shared; this row must assert the derived non-empty grant,
/// not parity with the pre-fix empty tracked set. Revert-publish fails this
/// even if the Arms `, then` rider is present.
#[test]
fn kayla_kindling_analog_exile_draft_attaches_cast_grant() {
    use crate::game::scenario::{GameScenario, P0};
    use crate::parser::oracle_effect::parse_effect_chain;
    use crate::types::ability::{
        AbilityCost, AbilityKind, CardPlayMode, CastingPermission, Duration,
    };
    use crate::types::phase::Phase;

    let def = parse_effect_chain(
        "draft a card from ~'s spellbook and exile it. Until end of turn, you may cast that card.",
        AbilityKind::Activated,
    )
    .cost(AbilityCost::Tap);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Kayla Analog", 1, 1)
        .with_ability_definition(def)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .spellbook = vec!["Lion Sash".to_string()];

    let outcome = runner
        .activate(source, 0)
        .spellbook_pick("Lion Sash")
        .resolve();

    let drafted = outcome
        .state()
        .objects
        .values()
        .find(|o| o.name == "Lion Sash")
        .expect("the drafted card must exist");
    assert_eq!(drafted.zone, Zone::Exile);
    let in_hand = outcome.state().players[0]
        .hand
        .iter()
        .filter_map(|id| outcome.state().objects.get(id))
        .any(|o| o.name == "Lion Sash");
    assert!(!in_hand, "Kayla analog must not create the card in hand");
    assert!(
        drafted.casting_permissions.iter().any(|perm| matches!(
            perm,
            CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfTurn,
                mode: CardPlayMode::Cast,
                ..
            }
        )),
        "derived reading: the grant must attach (mode Cast); empty permissions is the pre-fix defect, got {:?}",
        drafted.casting_permissions
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must return to Priority after the draft, got {:?}",
        outcome.final_waiting_for()
    );
}

/// Multi-authority hostile: two sources with different spellbooks and the
/// exile+grant chain. Picking from source A grants only the card conjured from
/// A's list; the permission's `source_id` is the resolving source.
#[test]
fn spellbook_exile_grant_binds_only_the_resolving_sources_pick() {
    use crate::game::scenario::{GameScenario, P0};
    use crate::parser::oracle_effect::parse_effect_chain;
    use crate::types::ability::{AbilityCost, AbilityKind, CastingPermission, Duration};
    use crate::types::phase::Phase;

    let draft_grant = || {
        parse_effect_chain(
            "draft a card from ~'s spellbook, then exile it. Until end of turn, you may play that card.",
            AbilityKind::Activated,
        )
        .cost(AbilityCost::Tap)
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source_a = scenario
        .add_creature(P0, "Archivist A", 1, 1)
        .with_ability_definition(draft_grant())
        .id();
    let source_b = scenario
        .add_creature(P0, "Archivist B", 1, 1)
        .with_ability_definition(draft_grant())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&source_a)
        .unwrap()
        .spellbook = vec!["Lion Sash".to_string(), "Alpha Two".to_string()];
    runner
        .state_mut()
        .objects
        .get_mut(&source_b)
        .unwrap()
        .spellbook = vec!["Beta One".to_string(), "Beta Two".to_string()];

    let outcome = runner
        .activate(source_a, 0)
        .spellbook_pick("Lion Sash")
        .resolve();

    let drafted = outcome
        .state()
        .objects
        .values()
        .find(|o| o.name == "Lion Sash")
        .expect("A's pick must be conjured");
    assert_eq!(drafted.zone, Zone::Exile);
    let grant = drafted
        .casting_permissions
        .iter()
        .find(|perm| {
            matches!(
                perm,
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    ..
                }
            )
        })
        .expect("the conjured card must receive the play grant");
    match grant {
        CastingPermission::PlayFromExile { source_id, .. } => {
            assert_eq!(
                *source_id,
                Some(source_a),
                "the grant must stamp the resolving source, not the other permanent"
            );
        }
        other => panic!("expected PlayFromExile grant, got {other:?}"),
    }
    assert!(
        outcome
            .state()
            .objects
            .values()
            .all(|o| o.name != "Beta One" && o.name != "Beta Two"),
        "source B's spellbook must not be conjured when A is resolving"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must return to Priority, got {:?}",
        outcome.final_waiting_for()
    );
}
