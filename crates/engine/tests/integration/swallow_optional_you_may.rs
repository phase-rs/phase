//! `Swallow:Optional_YouMay` regression: Hades, Sorcerer of Eld's labeled
//! graveyard-play permission must parse as the rules-bearing static it is, and
//! the same-class residuals must stay red-honest.
//!
//! Oracle (back face):
//!   "Vigilance
//!    Echo of the Lost — During your turn, you may play cards from your graveyard.
//!    If a card or token would be put into your graveyard from anywhere, exile it instead."
//!
//! "Echo of the Lost" is a flavor word (CR 207.2d): it has no rules meaning, but
//! "Echo" is also a CR 702.30a keyword-cost prefix, so the strict keyword-cost
//! router read the LABEL as a keyword declaration and fabricated
//! `Keyword::Echo(Mana { 0 })` from it — a fabricated keyword that then
//! synthesized a `TriggerMode::PayEcho` upkeep trigger — while the actual
//! "During your turn, you may play cards from your graveyard" permission was
//! never parsed at all. Declining a short spaced-dash label in
//! `parse_keyword_line_core` lets the line fall through to priority 14, which
//! strips the label and re-classifies the body as the static below.
//!
//! CR 601.2a + CR 305.1: the permission is `StaticMode::GraveyardCastPermission
//! { frequency: Unlimited, play_mode: Play }` gated by
//! `StaticCondition::DuringYourTurn` (CR 102.1), evaluated per priority window
//! by the casting pipeline — never a blanket active-player gate.
//! CR 614.1a: the back face's "exile it instead" is a `ReplacementEvent::Moved`
//! replacement.

use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{
    CardPlayMode, ContinuousModification, Effect, QuantityExpr, StaticCondition, TargetFilter,
    TypeFilter,
};
use engine::types::ability_visit::{
    visit_ability_def, visit_replacement, visit_static, visit_trigger,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use std::ops::ControlFlow;

use super::rules::AttackTarget;

const HADES_ORACLE: &str = "Vigilance\n\
    Echo of the Lost — During your turn, you may play cards from your graveyard.\n\
    If a card or token would be put into your graveyard from anywhere, exile it instead.";

/// Kethis, the Hidden Hand — its quoted grant is carried as
/// `ContinuousModification::AddStaticMode { mode: GraveyardCastPermission }`,
/// which `swallow_check::static_carries_optional_modification` does not yet
/// recognize, so the Optional_YouMay warning must survive (conservative-red).
const KETHIS_ORACLE: &str = "Legendary spells you cast cost {1} less to cast.\n\
    Exile two legendary cards from your graveyard: Until end of turn, each legendary card \
     in your graveyard gains \"You may play this card from your graveyard.\"";

/// Case of the Uneaten Feast — the Solved-ability quoted grant has the same
/// `AddStaticMode { GraveyardCastPermission }` shape as Kethis's.
const CASE_OF_THE_UNEATEN_FEAST_ORACLE: &str = "Whenever a creature you control enters, \
     you gain 1 life.\n\
     To solve — You've gained 5 or more life this turn. (If unsolved, solve at the \
     beginning of your end step.)\n\
     Solved — Sacrifice this Case: Creature cards in your graveyard gain \"You may cast \
     this card from your graveyard\" until end of turn.";

/// Drach'Nyen — a five-word flavor word ("Echo of the First Murder", CR 207.2d)
/// whose label also begins with the `echo` keyword-cost prefix. The wider
/// flavor-word cap (six words) must keep claiming the line as a trigger, and the
/// label decline must not reach it.
const DRACH_NYEN_ORACLE: &str = "Echo of the First Murder — When Drach'Nyen enters, exile \
     up to one target creature.\n\
     Daemon Sword — Equipped creature has menace and gets +X/+0, where X is the exiled \
     card's power.\n\
     Equip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)";

/// Emissary Green — the attack-triggered Council's-dilemma vote whose session
/// Ballot Broker's extra-vote static modifies. Production path, not a parse
/// fixture: the trigger, the `WaitingFor::VoteChoice` round-trips, and the
/// tally all run in `grants_extra_vote_session_forces_the_extra_ballot`.
const EMISSARY_GREEN_ORACLE: &str = "Whenever Emissary Green attacks, starting with you, \
     each player votes for profit or security. You create a number of Treasure tokens equal to \
     twice the number of profit votes. Put a number of +1/+1 counters on each creature you \
     control equal to the number of security votes.";

/// Ballot Broker — its `GrantsExtraVote` static allocates a second ballot to its
/// controller at vote-session start. The printed "you may" has no runtime
/// decline path, which is why these cards stay red-honest.
const BALLOT_BROKER_ORACLE: &str = "While voting, you may vote an additional time. (The votes \
     can be for different choices or for the same choice.)";

/// The production parse input for a multi-face card: `build_oracle_face_multi`
/// skips MTGJSON keywords (B8: cross-face keyword leakage) and substitutes the
/// `__force_keyword_extract__` sentinel, so this face's keywords must come from
/// the Oracle text itself. The inferred-hints path in `from_oracle_text` is not
/// production here: it would re-add keyword names from bare keyword lines and
/// could mask a mis-parse of the labeled line.
const PRODUCTION_KEYWORD_INPUT: &[&str] = &["__force_keyword_extract__"];

fn keyword_input() -> Vec<String> {
    PRODUCTION_KEYWORD_INPUT
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

fn has_swallowed_detector(parsed: &ParsedAbilities, detector: &str) -> bool {
    parsed.parse_warnings.iter().any(|warning| {
        matches!(
            warning,
            OracleDiagnostic::SwallowedClause {
                detector: warning_detector,
                ..
            } if warning_detector == detector
        )
    })
}

/// Does any ability tree in the parse contain an `Effect::Unimplemented`?
/// Walks the engine's single-authority visitor (`engine::types::ability_visit`)
/// over every top-level unit — abilities, trigger executes, statics, and
/// replacements — so an `Unimplemented` short-circuit in a nested carrier is
/// detected instead of leaving the swallow negatives vacuous.
fn tree_has_unimplemented(parsed: &ParsedAbilities) -> bool {
    let mut visitor = |effect: &Effect| {
        if matches!(effect, Effect::Unimplemented { .. }) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    parsed
        .abilities
        .iter()
        .any(|def| visit_ability_def(def, &mut visitor).is_break())
        || parsed
            .triggers
            .iter()
            .any(|trigger| visit_trigger(trigger, &mut visitor).is_break())
        || parsed
            .statics
            .iter()
            .any(|static_def| visit_static(static_def, &mut visitor).is_break())
        || parsed
            .replacements
            .iter()
            .any(|replacement| visit_replacement(replacement, &mut visitor).is_break())
}

/// Is the unit's rules-bearing graveyard permission carried as
/// `ContinuousModification::AddStaticMode { mode: GraveyardCastPermission }`
/// anywhere in the ability tree (the residual Kethis/Case shape)?
fn carries_add_static_mode_graveyard_permission(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(|def| match &*def.effect {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(|definition| {
            definition.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::GraveyardCastPermission { .. }
                    }
                )
            })
        }),
        _ => false,
    })
}

/// CR 601.2a: an illegal proposed cast is rejected and the card stays put.
fn assert_cast_rejected(runner: &mut GameRunner, card: ObjectId) {
    let card_id = runner.state().objects[&card].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: card,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::default(),
    });
    assert!(result.is_err(), "the cast must be rejected, got {result:?}");
    assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
}

fn offered_to(runner: &GameRunner, player: PlayerId, card: ObjectId) -> bool {
    let on_offer = spell_objects_available_to_cast(runner.state(), player).contains(&card);
    assert_eq!(
        on_offer,
        can_cast_object_now(runner.state(), player, card),
        "offer list and can_cast_object_now must agree for {card:?}"
    );
    on_offer
}

fn priority_holder(runner: &GameRunner) -> Option<PlayerId> {
    match runner.state().waiting_for {
        WaitingFor::Priority { player } => Some(player),
        _ => None,
    }
}

/// Pass priority until `active` is the active player and holds priority, then
/// (if `holder != active`) let the active player pass once so `holder` has
/// priority in that same step (CR 117.3d).
fn to_turn_with_priority(runner: &mut GameRunner, active: PlayerId, holder: PlayerId) {
    for _ in 0..40 {
        if runner.state().active_player == active && priority_holder(runner) == Some(active) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority toward the target turn");
    }
    assert_eq!(runner.state().active_player, active, "reach: turn reached");
    if holder != active {
        runner.act(GameAction::PassPriority).expect("active passes");
    }
    assert_eq!(
        priority_holder(runner),
        Some(holder),
        "reach: {holder:?} holds priority"
    );
}

fn instant_in_graveyard(scenario: &mut GameScenario, owner: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_spell_to_graveyard(owner, name, true)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn hades_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario
        .add_creature(P0, "Hades, Sorcerer of Eld", 6, 6)
        .as_legendary()
        .with_subtypes(vec!["Avatar"])
        .from_oracle_text_with_keywords(PRODUCTION_KEYWORD_INPUT, HADES_ORACLE);
    scenario
}

/// The label must not route as a keyword declaration: the back face's
/// graveyard-play permission parses as a `GraveyardCastPermission` static with
/// the CR 102.1 turn gate, the CR 614.1a exile-instead replacement is present,
/// `Vigilance` is still extracted — and no fabricated `Echo`/`PayEcho` pair
/// appears. Reverting the `parse_keyword_line_core` label decline restores the
/// fabricated `Keyword::Echo` + `TriggerMode::PayEcho` and drops the static, so
/// the assertions below flip.
#[test]
fn hades_back_face_echo_label_is_rules_free_static_not_keyword() {
    let scenario = hades_scenario();
    let runner = scenario.build();
    let hades = runner
        .state()
        .objects
        .iter()
        .find(|(_, object)| object.name == "Hades, Sorcerer of Eld")
        .map(|(id, _)| *id)
        .expect("Hades must be on the battlefield");

    let object = &runner.state().objects[&hades];

    // Reach guard: the labeled line is claimed by the static parser at all — the
    // permission exists as a typed static carrying the turn gate, not as an
    // Unimplemented stand-in.
    assert!(
        object
            .static_definitions
            .as_slice()
            .iter()
            .any(|definition| {
                matches!(
                    definition.mode,
                    StaticMode::GraveyardCastPermission {
                        frequency: CastFrequency::Unlimited,
                        play_mode: CardPlayMode::Play,
                        ..
                    }
                ) && definition.condition == Some(StaticCondition::DuringYourTurn)
            }),
        "expected GraveyardCastPermission {{ Unlimited, Play }} with DuringYourTurn; got {:#?}",
        object.static_definitions.as_slice()
    );

    // CR 614.1a: "If a card or token would be put into your graveyard from
    // anywhere, exile it instead." is a replacement, not a static.
    assert!(
        object
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|replacement| matches!(replacement.event, ReplacementEvent::Moved)),
        "the exile-instead replacement must be present, got {:#?}",
        object.replacement_definitions.as_slice()
    );

    // `Vigilance` is still extracted, and the fabricated Echo/PayEcho pair is gone.
    assert!(
        object
            .keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Vigilance)),
        "Vigilance must still be extracted, got {:?}",
        object.keywords
    );
    assert!(
        !object
            .keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Echo(_))),
        "the rules-free label must not fabricate Keyword::Echo, got {:?}",
        object.keywords
    );
    assert!(
        !object
            .base_trigger_definitions
            .iter()
            .any(|trigger| matches!(trigger.mode, TriggerMode::PayEcho)),
        "no PayEcho trigger may be synthesized from the label, got {:#?}",
        object.base_trigger_definitions
    );

    // The parse-warning surface: `from_oracle_text_with_keywords` discards
    // `parse_warnings`, so the same parser entry is exercised directly here with
    // the identical production inputs.
    let parsed = parse_oracle_text(
        HADES_ORACLE,
        "Hades, Sorcerer of Eld",
        &keyword_input(),
        &["Creature".to_string()],
        &["Avatar".to_string()],
    );
    assert!(
        parsed.statics.iter().any(|definition| matches!(
            definition.mode,
            StaticMode::GraveyardCastPermission { .. }
        )),
        "reach: the direct parse must produce the permission static, got {:#?}",
        parsed.statics
    );
    assert!(
        !has_swallowed_detector(&parsed, "Optional_YouMay"),
        "the labeled permission accounts for its own 'you may'; warnings: {:?}",
        parsed.parse_warnings
    );
}

/// CR 601.2a + CR 305.1 + CR 102.1: the permission is the controller's, gated on
/// the controller's turn — offered and usable on P0's turn, absent and rejected
/// on P1's turn.
#[test]
fn hades_graveyard_play_permission_is_limited_to_your_turns() {
    let mut scenario = hades_scenario();
    let first = instant_in_graveyard(&mut scenario, P0, "Graveyard Instant");
    let second = instant_in_graveyard(&mut scenario, P0, "Second Instant");
    let mut runner = scenario.build();

    assert!(
        offered_to(&runner, P0, first),
        "reach: offered on my own turn"
    );
    let outcome = runner.cast(first).resolve();
    outcome.assert_life_delta(P0, 1);
    assert_eq!(
        outcome.zone_of(first),
        Zone::Exile,
        "it resolved, and the back face's replacement exiles it instead"
    );

    to_turn_with_priority(&mut runner, P1, P0);
    assert!(
        !offered_to(&runner, P0, second),
        "\"during your turn\": not on the opponent's turn"
    );
    assert_cast_rejected(&mut runner, second);
}

/// CR 207.2d + CR 604.2 + CR 305.1: the quoted grants on Kethis and Case of the
/// Uneaten Feast are carried as `ContinuousModification::AddStaticMode`, which
/// `static_carries_optional_modification` does not yet recognize — so their
/// Optional_YouMay warnings must survive and keep the cards demoted.
/// Conservative-red by design: the fix's scope deliberately excludes the
/// `AddStaticMode` recognition arm, and dropping these warnings without that arm
/// would be a false green.
#[test]
fn kethis_and_case_of_the_uneaten_feast_stay_red_honest() {
    for (oracle, name, types) in [
        (KETHIS_ORACLE, "Kethis, the Hidden Hand", &["Creature"][..]),
        (
            CASE_OF_THE_UNEATEN_FEAST_ORACLE,
            "Case of the Uneaten Feast",
            &["Enchantment"][..],
        ),
    ] {
        let parsed = parse_oracle_text(
            oracle,
            name,
            &keyword_input(),
            &types.iter().map(|ty| (*ty).to_string()).collect::<Vec<_>>(),
            &[],
        );
        // Reach guard: the quoted grant IS parsed, as a typed permission carried by
        // `AddStaticMode` — the unit is not short-circuited by an Unimplemented, so
        // the surviving warning is a verdict on that carrier.
        assert!(
            carries_add_static_mode_graveyard_permission(&parsed),
            "{name}: the AddStaticMode {{ GraveyardCastPermission }} carrier must be present"
        );
        assert!(
            has_swallowed_detector(&parsed, "Optional_YouMay"),
            "{name}'s AddStaticMode-carried grant is not recognized as an optionality \
             carrier yet and must stay red: {:?}",
            parsed.parse_warnings
        );
    }
}

/// CR 207.2d: Drach'Nyen's five-word flavor word is beyond the label-decline
/// width, and trigger lines are claimed by the ability-word trigger route before
/// the keyword-cost router anyway — the labeled trigger body must still parse.
#[test]
fn labeled_trigger_body_still_parses() {
    let parsed = parse_oracle_text(
        DRACH_NYEN_ORACLE,
        "Drach'Nyen",
        &keyword_input(),
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
    assert!(
        parsed
            .triggers
            .iter()
            .any(|trigger| matches!(trigger.mode, TriggerMode::ChangesZone)),
        "the labeled enters trigger must parse, got {:#?}",
        parsed.triggers
    );

    // Reach guard: the whole parse is complete — no unit hides an
    // `Effect::Unimplemented` that would leave the `ChangesZone` mode green
    // while `check_swallowed_clauses` skips the unit, making the negative
    // below vacuous.
    assert!(
        !tree_has_unimplemented(&parsed),
        "reach: Drach'Nyen must parse with zero Unimplemented: {parsed:#?}"
    );

    // The labeled trigger's body is the real effect, not an Unimplemented
    // stand-in: "exile up to one target creature" parses as an Exile
    // `ChangeZone` over a Creature filter with a 0..=1 multi-target spec.
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| matches!(trigger.mode, TriggerMode::ChangesZone))
        .expect("reach: the labeled enters trigger must parse");
    let execute = trigger
        .execute
        .as_deref()
        .expect("reach: the labeled trigger must carry an execute body");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::Typed(type_filter),
                ..
            } if type_filter.type_filters.contains(&TypeFilter::Creature)
        ),
        "the labeled trigger body must be the real exile effect, got {:#?}",
        execute.effect
    );
    let multi_target = execute
        .multi_target
        .as_ref()
        .expect("reach: 'exile up to one target creature' must carry a MultiTargetSpec");
    assert_eq!(
        multi_target.min,
        QuantityExpr::Fixed { value: 0 },
        "'up to one' must allow zero targets"
    );
    assert_eq!(
        multi_target.max,
        Some(QuantityExpr::Fixed { value: 1 }),
        "'up to one' must cap the target count at one"
    );

    assert!(
        !parsed
            .parse_warnings
            .iter()
            .any(|warning| matches!(warning, OracleDiagnostic::SwallowedClause { .. })),
        "Drach'Nyen must parse without swallow findings: {:?}",
        parsed.parse_warnings
    );
}

/// CR 701.38d is a timing rule for a player who already has multiple votes
/// ("those votes all happen at the same time the player would otherwise have
/// voted"); it does not make the printed "While voting, you may vote an
/// additional time." optional. The runtime allocates the extra ballot
/// unconditionally — `game/effects/vote.rs`'s `votes_per_session_for` returns
/// `1 + extras` at vote-session start — and after every ballot
/// `engine_resolution_choices.rs`'s `append_vote_ballot_and_advance` re-opens
/// `WaitingFor::VoteChoice` for the same player whenever `remaining_votes > 1`.
/// There is no action that declines an allocated vote. This test drives that
/// production path (attack trigger → vote session → tally) with Ballot Broker
/// on the battlefield and pins the forced extra ballot: after P0's FIRST ballot
/// the session immediately re-prompts P0, which is exactly why the
/// `Optional_YouMay` warning on Ballot Broker stays red-honest.
#[test]
fn grants_extra_vote_session_forces_the_extra_ballot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let emissary = scenario
        .add_creature_from_oracle(P0, "Emissary Green", 3, 3, EMISSARY_GREEN_ORACLE)
        .id();
    scenario.add_creature_from_oracle(P0, "Ballot Broker", 2, 2, BALLOT_BROKER_ORACLE);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![emissary],
            valid_attack_targets: vec![AttackTarget::Player(P1)],
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(emissary, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declaring Emissary Green as attacker should succeed");

    // Pass priority until the attack-triggered vote prompt appears.
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::VoteChoice { .. }) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // Reach guard: Ballot Broker's `GrantsExtraVote` static reached the vote
    // session — P0 is allocated TWO ballots, not one.
    match runner.state().waiting_for {
        WaitingFor::VoteChoice {
            player,
            remaining_votes,
            ..
        } => {
            assert_eq!(player, P0, "P0 is the starting voter");
            assert_eq!(
                remaining_votes, 2,
                "reach: Ballot Broker's extra vote must be allocated"
            );
        }
        ref other => panic!("expected P0's first VoteChoice prompt, got {other:?}"),
    }

    // P0's FIRST ballot. The session must not advance to P1: the allocated
    // extra ballot is forced and immediately re-prompts the SAME player.
    runner
        .act(GameAction::ChooseOption {
            choice: "profit".to_string(),
        })
        .expect("P0's first ballot must be accepted");
    match runner.state().waiting_for {
        WaitingFor::VoteChoice {
            player,
            remaining_votes,
            ..
        } => {
            assert_eq!(
                player, P0,
                "the forced extra ballot belongs to the same player, immediately"
            );
            assert_eq!(
                remaining_votes, 1,
                "the second ballot is forced — no action declines it"
            );
        }
        ref other => {
            panic!("after P0's first ballot the session must force the extra ballot, got {other:?}")
        }
    }

    // The forced extra ballot is the only way forward: completing it (then
    // P1's ballot) is what lets the session resolve.
    runner
        .act(GameAction::ChooseOption {
            choice: "security".to_string(),
        })
        .expect("P0's forced second ballot must be accepted");
    let next_voter = match runner.state().waiting_for {
        WaitingFor::VoteChoice { player, .. } => player,
        ref other => panic!("expected P1's VoteChoice after P0's ballots, got {other:?}"),
    };
    assert_eq!(next_voter, P1, "the queue moves on only after both ballots");
    runner
        .act(GameAction::ChooseOption {
            choice: "profit".to_string(),
        })
        .expect("P1's ballot must be accepted");
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::VoteChoice { .. }),
        "all ballots cast — the session resolves, got {:?}",
        runner.waiting_for_kind()
    );
}
