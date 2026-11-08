//! Tests for the Alchemy spellbook draft (`Effect::DraftFromSpellbook`).
//! Declared from `effects/mod.rs` so `spellbook.rs` stays implementation-only.

use super::resolve_ability_chain;
use super::spellbook::{complete_draft, resolve};
use crate::game::zones::create_object;
use crate::parser::oracle::parse_oracle_text;
use crate::parser::oracle_effect::{parse_effect, parse_effect_chain};
use crate::types::ability::{
    AbilityKind, CardPlayMode, CastingPermission, Duration, Effect, QuantityExpr, ResolvedAbility,
    SpellbookSelection, TargetFilter,
};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{CardId, TrackedSetId};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

fn draft_ability(
    source: crate::types::identifiers::ObjectId,
    destination: Zone,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DraftFromSpellbook {
            destination,
            tapped: false,
            selection: SpellbookSelection::Draft,
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
    // Every draft wording is `SpellbookSelection::Draft`. Default → hand.
    // Trailing periods (as cards actually print) must still match.
    assert!(matches!(
        parse_effect("draft a card from Big Spender's spellbook."),
        Effect::DraftFromSpellbook {
            destination: Zone::Hand,
            tapped: false,
            selection: SpellbookSelection::Draft,
        }
    ));
    // Deferral pin (F1): the battlefield rider still creates the card directly
    // on the battlefield; the literal draft-into-hand-then-put reading is not
    // modeled yet.
    assert!(matches!(
        parse_effect(
            "draft a card from Adaptive Armorer's spellbook and put it onto the battlefield."
        ),
        Effect::DraftFromSpellbook {
            destination: Zone::Battlefield,
            tapped: false,
            selection: SpellbookSelection::Draft,
        }
    ));
    // CR 608.2c: both exile connectors (Kayla's ` and exile it`, Arms
    // Scavenger's and Tibalt's `, then exile it`) draft into hand, then exile
    // the drafted card. The texts are production-normalized: the card's
    // self-reference is `~`, so the whole clause reaches this leaf.
    for text in [
        "draft a card from ~'s spellbook and exile it.",
        "draft a card from ~'s spellbook, then exile it.",
    ] {
        let def = parse_effect_chain(text, AbilityKind::Spell);
        assert!(
            matches!(
                &*def.effect,
                Effect::DraftFromSpellbook {
                    destination: Zone::Hand,
                    tapped: false,
                    selection: SpellbookSelection::Draft,
                }
            ),
            "{text:?}: the draft goes to hand, got {:?}",
            def.effect
        );
        let exile = def
            .sub_ability
            .as_deref()
            .unwrap_or_else(|| panic!("{text:?}: the exile instruction must chain"));
        assert!(
            matches!(
                &*exile.effect,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::LastCreated,
                    ..
                }
            ),
            "{text:?}: \"exile it\" exiles the drafted card, got {:?}",
            exile.effect
        );
    }
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
            selection: SpellbookSelection::Draft,
        }
    ));
}

#[test]
fn parser_rejects_unmodeled_riders_as_unimplemented() {
    // "exile it face down", "twice, then …", and other unmodeled tails must NOT
    // collapse to a wrong effect — they fall through to a clean Unimplemented so
    // the coverage tooling flags them (and no clause is silently swallowed).
    // Reach-guard: the bare clause with the same head does parse to the draft,
    // so the rejections below are the rider's, not the head's.
    assert!(matches!(
        parse_effect("draft a card from this creature's spellbook."),
        Effect::DraftFromSpellbook {
            destination: Zone::Hand,
            selection: SpellbookSelection::Draft,
            ..
        }
    ));
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
                    selection: SpellbookSelection::Draft,
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
/// Reverting the `Random` resolver arm re-introduces the `SpellbookDraft` pause,
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
                    selection: SpellbookSelection::Random,
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
                selection: SpellbookSelection::Draft,
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
            // The draft offer is a random sample (its order is random), so
            // compare as sets; A's two-entry book is offered whole.
            let offered: std::collections::HashSet<&str> =
                options.iter().map(String::as_str).collect();
            assert_eq!(
                offered,
                std::collections::HashSet::from(["Alpha One", "Alpha Two"]),
                "only the activated source's spellbook may be offered, not the other permanent's"
            );
        }
        other => panic!("expected halt at SpellbookDraft, got {other:?}"),
    }
}

/// Verbatim Oracle of Arms Scavenger (MTGJSON AtomicCards).
const ARMS_SCAVENGER_ORACLE: &str = "At the beginning of your upkeep, draft a card from this creature's spellbook, then exile it. Until end of turn, you may play that card.\nEquip abilities you activate cost {1} less to activate.";

/// Verbatim Oracle of Tibalt, Wicked Tormentor (MTGJSON AtomicCards).
const TIBALT_WICKED_TORMENTOR_ORACLE: &str = "[+1]: Add {R}{R}. Draft a card from Tibalt, Wicked Tormentor's spellbook, then exile it. Until end of turn, you may cast that card.\n[+1]: Tibalt, Wicked Tormentor deals 4 damage to target creature or planeswalker unless its controller has Tibalt deal 4 damage to them. If they do, you may discard a card. If you do, draw a card.\n[\u{2212}X]: Create X 1/1 red Devil creature tokens with \"When this creature dies, it deals 1 damage to any target.\"";

/// Verbatim Oracle of Kayla's Kindling (MTGJSON AtomicCards).
const KAYLAS_KINDLING_ORACLE: &str = "When Kayla's Kindling enters, it deals 2 damage to any target.\nAt the beginning of your upkeep, draft a card from Kayla's Kindling's spellbook and exile it. Until end of turn, you may cast that card.";

/// Asserts no node of `def`'s sub-ability chain is `Effect::Unimplemented`.
fn assert_chain_has_no_unimplemented(def: &crate::types::ability::AbilityDefinition) {
    let mut node = Some(def);
    while let Some(current) = node {
        assert!(
            !matches!(&*current.effect, Effect::Unimplemented { .. }),
            "no instruction of the draft chain may be Unimplemented: {:?}",
            current.effect
        );
        node = current.sub_ability.as_deref();
    }
}

/// The literal draft-then-exile shape: `draft` is `DraftFromSpellbook { Hand,
/// Draft }`, its sub-ability exiles the drafted card (`LastCreated`), and that
/// node's sub-ability grants `mode` on "that card" (`TrackedSet(0)`) until end
/// of turn.
fn assert_draft_then_exile_then_grant(
    draft: &crate::types::ability::AbilityDefinition,
    mode: crate::types::ability::CardPlayMode,
) {
    assert!(
        matches!(
            &*draft.effect,
            Effect::DraftFromSpellbook {
                destination: Zone::Hand,
                tapped: false,
                selection: SpellbookSelection::Draft,
            }
        ),
        "the draft goes to its default zone, hand: {:?}",
        draft.effect
    );
    let exile = draft
        .sub_ability
        .as_deref()
        .expect("the exile instruction must chain after the draft");
    assert!(
        matches!(
            &*exile.effect,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::LastCreated,
                ..
            }
        ),
        "CR 608.2c: \"then exile it\" moves the drafted card to exile: {:?}",
        exile.effect
    );
    let grant = exile
        .sub_ability
        .as_deref()
        .expect("the that-card grant must chain after the exile");
    match &*grant.effect {
        Effect::GrantCastingPermission {
            permission:
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    mode: granted,
                    ..
                },
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            ..
        } => assert_eq!(*granted, mode),
        other => panic!(
            "expected GrantCastingPermission PlayFromExile UntilEndOfTurn TrackedSet(0), got {other:?}"
        ),
    }
}

/// The beginning-of-upkeep trigger's execute of a parsed card.
fn upkeep_execute(
    parsed: &crate::parser::oracle::ParsedAbilities,
) -> &crate::types::ability::AbilityDefinition {
    parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .expect("the card must parse an upkeep trigger")
        .execute
        .as_deref()
        .expect("the upkeep trigger must have an execute clause")
}

/// SHAPE (S1): production `parse_oracle_text` of Arms Scavenger lowers the
/// upkeep execute to the literal draft → exile → play-grant chain.
#[test]
fn arms_scavenger_parse_oracle_text_shape() {
    let parsed = parse_oracle_text(
        ARMS_SCAVENGER_ORACLE,
        "Arms Scavenger",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Warrior".to_string()],
    );
    let execute = upkeep_execute(&parsed);
    assert_chain_has_no_unimplemented(execute);
    assert_draft_then_exile_then_grant(execute, CardPlayMode::Play);
}

/// SHAPE (S2): Tibalt, Wicked Tormentor's first +1 — the mana, then the same
/// literal draft → exile chain with a **cast** grant.
#[test]
fn tibalt_wicked_tormentor_plus_one_parse_shape() {
    let parsed = parse_oracle_text(
        TIBALT_WICKED_TORMENTOR_ORACLE,
        "Tibalt, Wicked Tormentor",
        &[],
        &["Planeswalker".to_string()],
        &["Tibalt".to_string()],
    );
    let plus_one = parsed
        .abilities
        .first()
        .expect("Tibalt must parse its first loyalty ability");
    assert_chain_has_no_unimplemented(plus_one);
    assert!(
        matches!(&*plus_one.effect, Effect::Mana { .. }),
        "the +1 adds {{R}}{{R}} first: {:?}",
        plus_one.effect
    );
    let draft = plus_one
        .sub_ability
        .as_deref()
        .expect("the draft must chain after the mana");
    assert_draft_then_exile_then_grant(draft, CardPlayMode::Cast);
}

/// SHAPE (S3): Kayla's Kindling's " and exile it" connector takes the same
/// literal decomposition, with a cast grant.
#[test]
fn kaylas_kindling_parse_oracle_text_shape() {
    let parsed = parse_oracle_text(
        KAYLAS_KINDLING_ORACLE,
        "Kayla's Kindling",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let execute = upkeep_execute(&parsed);
    assert_chain_has_no_unimplemented(execute);
    assert_draft_then_exile_then_grant(execute, CardPlayMode::Cast);
}
