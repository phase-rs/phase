//! The Beamtown Bullies — "target opponent **whose turn it is**" (issue #6496).
//!
//! CR 102.1 defines the active player as "the player whose turn it is". CR 102.2
//! / CR 102.3 define opponent (team-aware). Together they mean this ability has
//! NO legal player target on its controller's own turn, which makes the
//! activation illegal to announce (CR 601.2c, imported into activation by
//! CR 602.2b; CR 602.2 makes the activation illegal when a step cannot be
//! complied with). CR 608.2b re-checks target legality on resolution, so the
//! reference must be read LIVE off `state.active_player`, never latched.
//!
//! This is implemented by parameterizing the existing controller reference as
//! `ControllerRef::ActivePlayer { relation: PlayerRelation }` — zero new enum
//! variants — so the narrowing rides the SAME candidate loop in
//! `targeting::find_legal_targets` that already applies the engine's
//! player-phasing exclusion (which mirrors CR 702.26b for permanents — the CR
//! has no player-phasing rule), left-the-game (CR 104.5), hexproof
//! (CR 702.11c), shroud (CR 702.18a) and protection (CR 702.16b).
//!
//! ---------------------------------------------------------------------------
//! KNOWN GAP — The Beamtown Bullies is NOT playable end-to-end after this change
//! and is deliberately left UNSUPPORTED. Its `{T}` ability still fails at
//! announce with `ActionNotAllowed("No legal targets available")`, for a
//! PRE-EXISTING reason unrelated to this fix:
//!
//!   `ability_utils::relative_controller_kind` (ability_utils.rs) classifies the
//!   chained `ChangeZone`'s `ControllerRef::You` ("from **your** graveyard") as a
//!   *relative* controller. When a companion player TARGET slot precedes it,
//!   `legal_targets_for_ability_filter` re-enumerates that filter bound to each
//!   candidate of the player slot (the "target player sacrifices a creature they
//!   control" mechanism). Beamtown's "your graveyard" is the ABILITY
//!   CONTROLLER's graveyard, not the target opponent's, so the re-enumeration
//!   searches the opponent's graveyard, finds nothing, and the whole activation
//!   is rejected.
//!
//! Verified pre-existing and independent of this change: the identical card text
//! with the relative clause REMOVED ("Target opponent puts target nonlegendary
//! creature card from your graveyard onto the battlefield under their control"),
//! which parses to the pre-change head filter `Typed { controller: Opponent }`,
//! fails with the exact same error. Two further riders are also still missing:
//! the "under their control" `enters_under` binding and parent -> child target
//! propagation in `resolve_ability_chain`. All three are out of scope for the
//! reviewed plan for this issue.
//!
//! Because the card is unplayable, it must NOT export as `supported: true,
//! gap_count: 0`. Two things hold that line, and both are pinned here:
//!   * `beamtown_activation_blocked_by_relative_your_graveyard_slot_known_gap`
//!     pins the runtime announce failure itself; and
//!   * `beamtown_change_zone_carries_enters_under_their_control_gap` pins the
//!     machine-readable marker — a strict-failure
//!     `Effect::unimplemented("enters_under_their_control", …)` node chained
//!     onto the move by `lower_subject_predicate_ast` — which is what makes
//!     `coverage-data.json` report the card as unsupported (CR 110.2a: dropping
//!     "under their control" would otherwise ship the permanent entering under
//!     the ABILITY controller's control, inverting the card).
//!
//! Both are KNOWN-GAP pins: UPDATE them when the blockers are fixed, never
//! delete them.
//!
//! So the runtime coverage below is deliberately split:
//!   * the CARD is covered at the parse + target-legality seams (the two seams
//!     this change actually moves), and
//!   * the CLASS is covered end-to-end through the real `apply()` activation
//!     pipeline using a minimal card that exercises the same seam without the
//!     unrelated chained-`You` blocker.
//! ---------------------------------------------------------------------------

use engine::game::engine::EngineError;
use engine::game::scenario::GameScenario;
use engine::game::targeting::find_legal_targets;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, PlayerRelation, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{GameAction, PlayerId};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// Verbatim Scryfall Oracle text for The Beamtown Bullies (verified against the
/// Scryfall API — never paraphrased).
const BEAMTOWN_ORACLE: &str = "Vigilance, haste\n{T}: Target opponent whose turn it is puts target nonlegendary creature card from your graveyard onto the battlefield under their control. It gains haste. Goad it. At the beginning of the next end step, exile it. (Until your next turn, that creature attacks each combat if able and attacks a player other than you if able.)";

/// Verbatim Scryfall Oracle text for Cruel Edict — a shipped "target opponent"
/// card with NO relative clause, used as the byte-identity regression.
const CRUEL_EDICT_ORACLE: &str = "Target opponent sacrifices a creature of their choice.";

/// Minimal CLASS card: the same "target opponent whose turn it is" player-target
/// grammar with a one-step predicate, so the full activation pipeline is
/// reachable without the unrelated chained-`You` blocker described above.
const ACTIVE_OPPONENT_CLASS_ORACLE: &str = "{T}: Target opponent whose turn it is loses 1 life.";

fn parse_beamtown() -> Vec<AbilityDefinition> {
    parse_oracle_text(
        BEAMTOWN_ORACLE,
        "The Beamtown Bullies",
        &["Vigilance".to_string(), "Haste".to_string()],
        &["Creature".to_string()],
        &[
            "Ogre".to_string(),
            "Devil".to_string(),
            "Warrior".to_string(),
        ],
    )
    .abilities
}

/// Walk an ability's `sub_ability` chain looking for `Effect::Unimplemented`.
fn chain_has_unimplemented(def: &AbilityDefinition) -> bool {
    !chain_unimplemented_names(def).is_empty()
}

/// Collect the `name` of every `Effect::Unimplemented` node on an ability's
/// `sub_ability` chain, in chain order.
fn chain_unimplemented_names(def: &AbilityDefinition) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = Some(def);
    while let Some(node) = current {
        if let Effect::Unimplemented { name, .. } = node.effect.as_ref() {
            names.push(name.clone());
        }
        current = node.sub_ability.as_deref();
    }
    names
}

/// CR 115.1 + CR 102.1 + CR 102.2: the verbatim card text, driven through the
/// real top-level `parse_oracle_text` entry point, binds the player target to
/// the ACTIVE-OPPONENT legality scope.
///
/// Revert-failing: with the `parse_active_player_turn_clause` wiring removed the
/// controller parses to the unnarrowed `ControllerRef::Opponent`, so the
/// `assert_eq!` below fails.
///
/// The paired reach-guard pins the chain's gap inventory EXACTLY — one node,
/// the deliberate `enters_under_their_control` KNOWN-GAP marker (see
/// `beamtown_change_zone_carries_enters_under_their_control_gap`). Naming it
/// rather than asserting "no gaps at all" keeps the guard load-bearing: a
/// degraded parse would either add a second name or replace this one, and
/// either way the assertion fails.
#[test]
fn beamtown_verbatim_binds_active_opponent_player_target() {
    let abilities = parse_beamtown();
    let activated = abilities
        .iter()
        .find(|a| a.cost.is_some())
        .expect("the {T} activated ability must parse");

    assert_eq!(
        chain_unimplemented_names(activated),
        vec!["enters_under_their_control".to_string()],
        "reach-guard: the {{T}} chain must carry exactly the one deliberate gap, got {:?}",
        activated.effect
    );

    assert_eq!(
        activated.effect.target_filter(),
        Some(&TargetFilter::Typed(TypedFilter::default().controller(
            ControllerRef::ActivePlayer {
                relation: PlayerRelation::Opponent,
            }
        ))),
        "the head node's player target must carry the active-opponent legality scope"
    );
}

/// KNOWN-GAP PIN — **update this test, do not delete it**, in the change that
/// gives `Effect::ChangeZone::enters_under` a target-slot carrier.
///
/// CR 110.2a: an object a player is INSTRUCTED to put onto the battlefield
/// enters under that player's control. Here that player is the clause subject —
/// the targeted active opponent — and the printed "under their control" restates
/// it. The engine has no `ControllerRef` for "the player bound to this ability's
/// target slot", so the binding is dropped and `enters_under: None` falls back
/// to the ABILITY CONTROLLER, inverting the card.
///
/// `lower_subject_predicate_ast` therefore chains a strict-failure
/// `Effect::unimplemented("enters_under_their_control", …)` node onto the move,
/// which is what keeps this card OUT of the supported bucket in
/// `coverage-data.json` (`supported: false`, `gap_count >= 1`) instead of
/// exporting an inverted implementation as fully handled.
///
/// REMOVAL ORDER IS LOAD-BEARING: the gap node must be deleted in the SAME
/// change that gives `enters_under` a target-slot carrier and unblocks
/// activation — never in a later cleanup. Inserting it makes the `ChangeZone`'s
/// immediate `sub_link` a `ContinuationStep` (`AbilityDefinition::new`'s
/// default) where the printed chain would otherwise hand the next instruction a
/// `SequentialSibling` hop. Nothing reads that link from a `ChangeZone` parent
/// today because the ability cannot be announced at all, so there is no live
/// defect — but a change that unblocks activation while leaving this node in
/// place would make the haste grant / `Goad` / delayed exile resolve one hop
/// deeper than parsed.
///
/// Reach-guards, so this cannot pass on a collapsed parse: the `ChangeZone` the
/// gap qualifies is still present and still carries its real origin,
/// destination and moved-object filter — the gap node is an ADDITIONAL rider,
/// not a replacement for the parse.
#[test]
fn beamtown_change_zone_carries_enters_under_their_control_gap() {
    let abilities = parse_beamtown();
    let activated = abilities
        .iter()
        .find(|a| a.cost.is_some())
        .expect("the {T} activated ability must parse");

    // Reach-guard: the head is still the player target slot, not a gap node.
    assert!(
        matches!(activated.effect.as_ref(), Effect::TargetOnly { .. }),
        "reach-guard: the head node must stay the player target slot, got {:?}",
        activated.effect
    );

    let move_node = activated
        .sub_ability
        .as_deref()
        .expect("the move is chained under the player target slot");
    // Reach-guard: the move itself survived intact.
    let Effect::ChangeZone {
        origin,
        destination,
        enters_under,
        ..
    } = move_node.effect.as_ref()
    else {
        panic!(
            "reach-guard: the graveyard -> battlefield move must survive, got {:?}",
            move_node.effect
        );
    };
    assert_eq!(*origin, Some(Zone::Graveyard));
    assert_eq!(*destination, Zone::Battlefield);
    // The gap itself: no controller override was bound.
    assert_eq!(
        *enters_under, None,
        "\"under their control\" has no ControllerRef carrier yet"
    );

    // The strict-failure marker rides directly on the move it qualifies.
    let gap = move_node
        .sub_ability
        .as_deref()
        .expect("the gap node is chained onto the move");
    let Effect::Unimplemented { name, description } = gap.effect.as_ref() else {
        panic!(
            "the unbound \"under their control\" clause must be recorded as a gap, got {:?}",
            gap.effect
        );
    };
    assert_eq!(name, "enters_under_their_control");
    // The fragment is the UNBOUND TAIL only, not the whole predicate: the move
    // itself parsed and exports `supported: true`, so pointing coverage at the
    // full "put target … onto the battlefield …" text would render
    // already-implemented Oracle text as unsupported.
    assert_eq!(
        description.as_deref(),
        Some("under their control"),
        "the gap must carry exactly the unbound controller binding"
    );

    // Reach-guard: the rest of the printed chain still hangs off the gap node —
    // the marker is a rider, not a truncation.
    assert!(
        gap.sub_ability.is_some(),
        "the remaining printed instructions must still be chained"
    );
}

/// CR 601.2c + CR 602.2 + CR 608.2b: the PARSED filter, evaluated through the
/// production target-legality authority (`targeting::find_legal_targets`, the
/// function `collect_target_slots` calls for every slot), yields exactly the
/// active opponent — and nothing at all on the controller's own turn.
///
/// Revert-failing: with the `targeting.rs` `relation` conjunction removed, the
/// own-turn case returns `[Player(P0)]` (the controller!) and the three-player
/// case is unchanged, so the first two assertions flip.
#[test]
fn beamtown_parsed_filter_is_live_and_active_opponent_scoped() {
    let abilities = parse_beamtown();
    let filter = abilities
        .iter()
        .find(|a| a.cost.is_some())
        .and_then(|a| a.effect.target_filter())
        .expect("the {T} ability surfaces a player target filter")
        .clone();

    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
    let bullies = engine::game::zones::create_object(
        &mut state,
        CardId(1),
        P0,
        "The Beamtown Bullies".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&bullies)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    // CR 102.1: on an opponent's turn, exactly that opponent is legal.
    state.active_player = P1;
    assert_eq!(
        find_legal_targets(&state, &filter, P0, bullies),
        vec![TargetRef::Player(P1)],
        "only the active opponent is a legal target"
    );

    // CR 608.2b: the reference is LIVE — moving the turn moves the legal target.
    state.active_player = P2;
    assert_eq!(
        find_legal_targets(&state, &filter, P0, bullies),
        vec![TargetRef::Player(P2)],
        "the legal target follows the live active player, never a latched value"
    );

    // CR 601.2c + CR 602.2: on the controller's own turn there is no legal
    // target at all, so the activation cannot be announced.
    state.active_player = P0;
    assert_eq!(
        find_legal_targets(&state, &filter, P0, bullies),
        vec![],
        "no opponent is the active player on the controller's own turn"
    );

    // Reach-guard for the empty result above: the SAME state, queried with the
    // unnarrowed opponent filter, still enumerates both opponents. The emptiness
    // is the `relation` narrowing, not a broken fixture.
    let unnarrowed =
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
    assert_eq!(
        find_legal_targets(&state, &unnarrowed, P0, bullies),
        vec![TargetRef::Player(P1), TargetRef::Player(P2)],
        "reach-guard: two opponents exist and are otherwise targetable"
    );
}

/// Byte-identity regression: a SHIPPED "target opponent" card with no relative
/// clause, driven through the real `parse_oracle_text` pipeline, is unchanged by
/// the new guarded clause attempt.
///
/// Reach-guard: zero `Effect::Unimplemented`, so the shape assertion is made on
/// a card that genuinely parsed.
#[test]
fn shipped_target_opponent_card_parse_is_unchanged() {
    let parsed = parse_oracle_text(
        CRUEL_EDICT_ORACLE,
        "Cruel Edict",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1, "one spell ability");
    let def = &parsed.abilities[0];
    assert!(
        !chain_has_unimplemented(def),
        "reach-guard: Cruel Edict must fully parse, got {:?}",
        def.effect
    );
    // Byte-identical to the pre-change parse: the "target opponent" head noun
    // still routes through the `TargetOpponent` companion-slot machinery.
    assert_eq!(
        def.effect.target_filter(),
        Some(&TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::TargetPlayer)
        )),
        "an unqualified `target opponent` must keep its pre-existing scope"
    );
    assert!(
        !format!("{:?}", def.effect).contains("ActivePlayer"),
        "no ActivePlayer scope may leak into a card without a turn-ownership clause"
    );
}

/// CLASS-level runtime coverage through the real `apply()` activation pipeline
/// (`GameAction::ActivateAbility` -> `collect_target_slots` ->
/// `WaitingFor::TargetSelection`).
///
/// CR 102.1 + CR 102.2: with two opponents, the ability's single legal player
/// target is EXACTLY the active opponent — not both opponents, and never the
/// controller. Because exactly one candidate is legal, the engine auto-declares
/// it (CR 601.2c) and the ability goes straight to the stack bound to P2.
///
/// Revert-failing on two axes. With the `relation` conjunction removed from
/// `targeting.rs`, BOTH opponents are legal, so (a) the ability halts on a
/// `TargetSelection` prompt instead of `Priority` and the auto-declared-target
/// assertion below fails, and (b) the resolved life loss can land on P1.
#[test]
fn active_opponent_class_activation_binds_only_the_turn_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = {
        let mut b = scenario.add_creature(P0, "Active Opponent Punisher", 1, 1);
        b.from_oracle_text(ACTIVE_OPPONENT_CLASS_ORACLE);
        b.id()
    };
    let mut runner = scenario.build();
    runner.state_mut().active_player = P2;

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activation must be legal while an opponent is the active player");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "exactly one legal target means no prompt is raised, got {:?}",
        runner.state().waiting_for
    );
    let entry = runner
        .state()
        .stack
        .back()
        .expect("the activated ability is on the stack");
    let engine::types::game_state::StackEntryKind::ActivatedAbility { ability, .. } = &entry.kind
    else {
        panic!(
            "expected an activated ability on the stack, got {:?}",
            entry.kind
        );
    };
    assert_eq!(
        ability.targets,
        vec![TargetRef::Player(P2)],
        "only the ACTIVE opponent is bound — not the other opponent, not the controller"
    );

    // Drive the resolution through the pipeline: the life loss lands on P2 only.
    runner.advance_until_stack_empty();
    let life = |p: PlayerId| {
        runner
            .state()
            .players
            .iter()
            .find(|x| x.id == p)
            .unwrap()
            .life
    };
    assert_eq!(life(P2), 19, "the active opponent loses 1 life");
    assert_eq!(life(P1), 20, "the non-active opponent is untouched");
    assert_eq!(life(P0), 20, "the controller is untouched");
}

/// KNOWN-GAP PIN — **update this test, do not delete it**, in the change that
/// fixes the underlying blocker.
///
/// The Beamtown Bullies' `{T}` ability is NOT activatable today, even with every
/// precondition satisfied: an opponent is the active player, the source is
/// untapped and not summoning-sick, and a legal nonlegendary creature card is
/// sitting in the ABILITY CONTROLLER's graveyard. The announce fails with
/// `EngineError::ActionNotAllowed("No legal targets available")`.
///
/// Cause (PRE-EXISTING, unrelated to the active-player work in this change):
/// `ability_utils::relative_controller_kind` classifies the chained
/// `ChangeZone`'s `ControllerRef::You` — the "from **your** graveyard" scope —
/// as a *relative* controller. Because a companion player TARGET slot precedes
/// it, `legal_targets_for_ability_filter` re-enumerates that filter bound to
/// each candidate of the player slot (the "target player sacrifices a creature
/// they control" mechanism), so it searches the TARGET OPPONENT's graveyard,
/// finds nothing, and rejects the whole activation.
///
/// This test exists so that behaviour cannot silently change. When the blocker
/// is fixed, this test must be rewritten into the positive end-to-end coverage
/// it is standing in for — not removed.
///
/// Non-vacuity is guaranteed by three reach-guards in the SAME fixture:
///   * the `{T}` cost is payable (untapped, not summoning-sick);
///   * the moved-object candidate really is a nonlegendary creature card in the
///     controller's graveyard;
///   * the PLAYER slot's own filter, run through the production legality
///     authority `targeting::find_legal_targets`, enumerates exactly the active
///     opponent — so the failure is provably the graveyard slot, not the player
///     slot this change actually moves.
#[test]
fn beamtown_activation_blocked_by_relative_your_graveyard_slot_known_gap() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = {
        let mut b = scenario.add_creature(P0, "The Beamtown Bullies", 3, 3);
        b.from_oracle_text(BEAMTOWN_ORACLE);
        b.id()
    };
    // "…target nonlegendary creature card from your graveyard…" — a legal
    // candidate for the moved-object slot, in the ABILITY CONTROLLER's graveyard.
    let corpse = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;

    // Reach-guard (cost payability).
    let obj = runner.state().objects.get(&source).unwrap();
    assert!(!obj.tapped, "reach-guard: the source is untapped");
    assert!(
        !obj.summoning_sick,
        "reach-guard: the source is not summoning-sick"
    );

    // Reach-guard (moved-object candidate): a nonlegendary creature card really
    // is in the controller's graveyard.
    let corpse_obj = runner
        .state()
        .objects
        .get(&corpse)
        .expect("the graveyard candidate exists");
    assert_eq!(
        corpse_obj.zone,
        Zone::Graveyard,
        "reach-guard: the candidate is in a graveyard"
    );
    assert_eq!(
        corpse_obj.controller, P0,
        "reach-guard: it is the ABILITY CONTROLLER's graveyard"
    );
    assert!(
        corpse_obj
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach-guard: the candidate is a creature card"
    );

    // Reach-guard (the player slot works): the head node's own filter, run
    // through the production legality authority, yields exactly the active
    // opponent. Whatever fails below, it is not this slot.
    let player_filter = parse_beamtown()
        .iter()
        .find(|a| a.cost.is_some())
        .and_then(|a| a.effect.target_filter())
        .expect("the {T} ability surfaces a player target filter")
        .clone();
    assert_eq!(
        find_legal_targets(runner.state(), &player_filter, P0, source),
        vec![TargetRef::Player(P1)],
        "reach-guard: the PLAYER slot enumerates exactly the active opponent"
    );

    // The pinned failure: the graveyard slot is re-enumerated against the
    // targeted opponent, so no candidate is found and announce is rejected.
    let result = runner.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: 0,
    });
    let Err(EngineError::ActionNotAllowed(message)) = result else {
        panic!(
            "KNOWN GAP: the {{T}} ability is expected to be unactivatable today, got {result:?}. \
             If the `relative_controller_kind` blocker was fixed, UPDATE this test into positive \
             end-to-end coverage — do not delete it."
        );
    };
    assert_eq!(
        message, "No legal targets available",
        "the pinned failure is the empty moved-object slot"
    );
}

/// CR 601.2c + CR 602.2: on the controller's own turn the ability has no legal
/// target, so the activation is illegal.
///
/// Mandatory paired reach-guards, in the SAME fixture, so the negative is not
/// vacuous:
///   * the source is untapped and not summoning-sick (the `{T}` cost is payable);
///   * flipping ONLY `state.active_player` to an opponent makes the identical
///     activation succeed.
#[test]
fn active_opponent_class_activation_illegal_on_controllers_own_turn() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = {
        let mut b = scenario.add_creature(P0, "Active Opponent Punisher", 1, 1);
        b.from_oracle_text(ACTIVE_OPPONENT_CLASS_ORACLE);
        b.id()
    };
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;

    // Reach-guard (cost payability): the `{T}` cost can actually be paid.
    let obj = runner.state().objects.get(&source).unwrap();
    assert!(!obj.tapped, "reach-guard: the source is untapped");
    assert!(
        !obj.summoning_sick,
        "reach-guard: the source is not summoning-sick"
    );

    let result = runner.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: 0,
    });
    assert!(
        result.is_err(),
        "no opponent is the active player, so the activation must be illegal, got {result:?}"
    );

    // Reach-guard (the ONLY difference is whose turn it is): with an opponent
    // active, the identical activation is accepted.
    runner.state_mut().active_player = P1;
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("reach-guard: the same ability activates once an opponent is active");
}

/// CR 102.3: in a team game a player's opponents are only players NOT on their
/// team, so an active TEAMMATE is not "the opponent whose turn it is". This is
/// the only runtime test that distinguishes `players::matches_relation` from a
/// bare `player.id != controller` inequality.
#[test]
fn active_opponent_class_activation_excludes_two_headed_giant_teammate() {
    // Seats: P0+P1 one team, P2+P3 the other.
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = {
        let mut b = scenario.add_creature(P0, "Active Opponent Punisher", 1, 1);
        b.from_oracle_text(ACTIVE_OPPONENT_CLASS_ORACLE);
        b.id()
    };
    let mut runner = scenario.build();

    runner.state_mut().active_player = P1;
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .is_err(),
        "an active TEAMMATE is not an opponent (CR 102.3), so no legal target exists"
    );

    // Reach-guard: an active opposing-team player IS an opponent.
    runner.state_mut().active_player = PlayerId(2);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("reach-guard: an opposing-team active player is a legal target");
}

/// CR 608.2b: the announced player target is re-checked on RESOLUTION against
/// the LIVE `state.active_player`, never against a value latched at announce.
///
/// End-to-end through the real `apply()` pipeline (the CLASS card — The Beamtown
/// Bullies itself still cannot be announced, see
/// `beamtown_activation_blocked_by_relative_your_graveyard_slot_known_gap`).
/// Both halves run on the SAME fixture shape, so neither can pass vacuously:
///   * the target stays the active player through resolution -> the ability
///     resolves and the life loss lands; and
///   * the turn passes to a DIFFERENT player between announce and resolution ->
///     the sole target is no longer legal, so it is removed and the ability is
///     countered on resolution (no life loss anywhere).
///
/// The resolution path also runs the target refs back through
/// `targeting::target_ref_matches_resolved_filter` ->
/// `filter::player_matches_target_filter_in_state`, which is where the
/// `ControllerRef::ActivePlayer { relation }` arm has to read state instead of
/// failing closed. Note this test is a BEHAVIOURAL pin, not the regression pin
/// for that arm: the fizzle itself is decided by
/// `targeting::find_legal_targets`, which resolves the relation independently.
/// `active_opponent_filter_matches_only_the_live_active_opponent_in_state`
/// below is the revert-failing pin on the matcher.
#[test]
fn active_opponent_target_is_revalidated_against_the_live_active_player() {
    let life_of = |runner: &engine::game::scenario::GameRunner, p: PlayerId| {
        runner
            .state()
            .players
            .iter()
            .find(|x| x.id == p)
            .expect("player exists")
            .life
    };

    let build = || {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = {
            let mut b = scenario.add_creature(P0, "Active Opponent Punisher", 1, 1);
            b.from_oracle_text(ACTIVE_OPPONENT_CLASS_ORACLE);
            b.id()
        };
        let mut runner = scenario.build();
        runner.state_mut().active_player = P2;
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .expect("activation must be legal while P2 is the active player");
        let entry = runner
            .state()
            .stack
            .back()
            .expect("the activated ability is on the stack");
        let engine::types::game_state::StackEntryKind::ActivatedAbility { ability, .. } =
            &entry.kind
        else {
            panic!("expected an activated ability on the stack");
        };
        assert_eq!(
            ability.targets,
            vec![TargetRef::Player(P2)],
            "reach-guard: the announced target is the active opponent"
        );
        runner
    };

    // POSITIVE half: the target is still the active player at resolution.
    let mut still_active = build();
    still_active.advance_until_stack_empty();
    assert_eq!(
        life_of(&still_active, P2),
        19,
        "the target is still the active opponent on resolution, so the ability resolves"
    );

    // NEGATIVE half: the turn passed to P1 between announce and resolution.
    let mut turn_passed = build();
    turn_passed.state_mut().active_player = P1;
    turn_passed.advance_until_stack_empty();
    assert_eq!(
        life_of(&turn_passed, P2),
        20,
        "P2 is no longer the active player, so the sole target is illegal and the ability \
         is countered on resolution (CR 608.2b)"
    );
    assert_eq!(
        life_of(&turn_passed, P1),
        20,
        "the fizzled ability must not slide onto the NEW active player either"
    );
    assert_eq!(life_of(&turn_passed, P0), 20, "the controller is untouched");
}

/// Direct assertion on the resolution-side matcher authority
/// (`filter::player_matches_target_filter_in_state`), so the
/// `ControllerRef::ActivePlayer { relation }` arm cannot silently regress back
/// to a fail-closed `false` — which would make every "target opponent whose
/// turn it is" ability fail its own CR 608.2b re-check while the target is
/// still perfectly legal.
///
/// Also pins the state-free sibling `filter::player_matches_target_filter`,
/// which has no `GameState` and therefore MUST stay fail-closed for this arm
/// (CR 102.1 — "the player whose turn it is" is unanswerable without state).
///
/// Every negative below is paired with a positive on the same fixture.
#[test]
fn active_opponent_filter_matches_only_the_live_active_opponent_in_state() {
    use engine::game::filter::{
        player_matches_target_filter, player_matches_target_filter_in_state,
    };

    let filter = TargetFilter::Typed(TypedFilter::default().controller(
        ControllerRef::ActivePlayer {
            relation: PlayerRelation::Opponent,
        },
    ));

    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    state.active_player = P2;

    // Positive: the live active player, and an opponent of the source's
    // controller (CR 102.1 + CR 102.2).
    assert!(
        player_matches_target_filter_in_state(&state, &filter, P2, Some(P0)),
        "the active opponent must still match on resolution"
    );
    // Negative: an opponent who is NOT the active player.
    assert!(
        !player_matches_target_filter_in_state(&state, &filter, P1, Some(P0)),
        "a non-active opponent is not 'the opponent whose turn it is'"
    );
    // Negative: the source's own controller, even when they are active.
    assert!(
        !player_matches_target_filter_in_state(&state, &filter, P0, Some(P0)),
        "the controller is never their own opponent (CR 102.2)"
    );

    // Negative: on the controller's own turn nobody matches...
    state.active_player = P0;
    assert!(
        !player_matches_target_filter_in_state(&state, &filter, P2, Some(P0)),
        "on the controller's own turn the active player is not an opponent"
    );
    // ...and the reach-guard: moving ONLY the active player back makes P2 match
    // again, so the negative above is about whose turn it is and nothing else.
    state.active_player = P2;
    assert!(
        player_matches_target_filter_in_state(&state, &filter, P2, Some(P0)),
        "reach-guard: the sole difference is `state.active_player`"
    );

    // The state-free entry point stays fail-closed for this arm...
    assert!(
        !player_matches_target_filter(&filter, P2, Some(P0)),
        "without a GameState there is no active player to read — fail closed"
    );
    // ...and the reach-guard that it is not simply broken for all player
    // filters: the unnarrowed opponent scope still matches there.
    assert!(
        player_matches_target_filter(
            &TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            P2,
            Some(P0)
        ),
        "reach-guard: the state-free matcher still resolves plain opponent scope"
    );
}

/// CR 102.3: the in-state matcher's `relation` narrowing is team-aware — an
/// active TEAMMATE is not "the opponent whose turn it is". Same authority
/// (`players::active_player_satisfies_relation`) the announce-time enumeration
/// uses, asserted directly so the two seams cannot drift apart.
#[test]
fn active_opponent_filter_in_state_excludes_two_headed_giant_teammate() {
    use engine::game::filter::player_matches_target_filter_in_state;

    let filter = TargetFilter::Typed(TypedFilter::default().controller(
        ControllerRef::ActivePlayer {
            relation: PlayerRelation::Opponent,
        },
    ));

    // Seats: P0+P1 one team, P2+P3 the other.
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);

    state.active_player = P1;
    assert!(
        !player_matches_target_filter_in_state(&state, &filter, P1, Some(P0)),
        "an active TEAMMATE is not an opponent (CR 102.3)"
    );
    // Reach-guard: an active opposing-team player IS.
    state.active_player = P2;
    assert!(
        player_matches_target_filter_in_state(&state, &filter, P2, Some(P0)),
        "reach-guard: an opposing-team active player matches"
    );
}

/// Serde migration guard. `ControllerRef` is externally tagged, so
/// parameterizing `ActivePlayer` changed its wire form from the bare string
/// `"ActivePlayer"` to `{"ActivePlayer":{"relation":{"type":"…"}}}`. Pins the
/// exact shape the regenerated card-data / fixture export must carry, and
/// round-trips both inhabited relations.
#[test]
fn controller_ref_active_player_serde_round_trip() {
    for relation in [
        PlayerRelation::All,
        PlayerRelation::Opponent,
        PlayerRelation::Controller,
    ] {
        let value = ControllerRef::ActivePlayer { relation };
        let json = serde_json::to_string(&value).expect("serializes");
        let back: ControllerRef = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(back, value, "round-trip mismatch for {relation:?}");
    }

    // The exact shape the migrated `tests/fixtures/integration_cards.json`
    // carries (and that `scripts/gen-test-fixture.py` regenerates).
    assert_eq!(
        serde_json::to_string(&ControllerRef::ActivePlayer {
            relation: PlayerRelation::All,
        })
        .unwrap(),
        r#"{"ActivePlayer":{"relation":{"type":"All"}}}"#
    );

    // Sibling non-regression: the other unit variants keep their bare-string form.
    assert_eq!(
        serde_json::to_string(&ControllerRef::Opponent).unwrap(),
        r#""Opponent""#
    );
}

/// Guard the fixture migration itself: the committed integration fixture must
/// deserialize with the new shape. `support::shared_card_db()` parses it, so a
/// missed occurrence would fail at RUNTIME (not compile time) for every
/// fixture-backed test; this test names the failure.
#[test]
fn integration_fixture_still_loads_after_controller_ref_migration() {
    let db = crate::support::shared_card_db().expect("the committed fixture must be present");
    assert!(
        db.card_count() > 0,
        "the committed integration fixture must still deserialize"
    );
    // Reach-guard: the fixture genuinely contains a migrated `ActivePlayer`
    // entry, so a silently-empty fixture cannot make this vacuous.
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/integration_cards.json"
    ))
    .expect("fixture is readable");
    assert!(
        raw.contains(r#"{"ActivePlayer":{"relation":{"type":"All"}}}"#),
        "the fixture must carry the migrated ActivePlayer wire shape"
    );
    assert!(
        !raw.contains(r#""controller":"ActivePlayer""#),
        "no legacy bare-string ActivePlayer may remain in the fixture"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// GUARD-NEGATIVES for the `enters_under_their_control` strict-failure node.
//
// The guard in `lower_subject_predicate_ast` is a conjunction — `ChangeZone` /
// `ChangeZoneAll`, `destination == Battlefield`, `enters_under == None`, and a
// word-boundary scan for the exact phrase "under their control". The positive
// pins above only prove it FIRES. These prove it stays SILENT, so a future edit
// that widens the phrase scan or drops a conjunct cannot quietly start marking
// correctly-parsed cards `supported: false` (or, in the inverse direction, hide
// a real controller inversion behind an over-narrow test suite).
//
// Every one of these drives the full `parse_oracle_text` pipeline and asserts
// BOTH directions: zero `Effect::Unimplemented` anywhere in the chain, AND a
// positive reach-guard on the `ChangeZone` that proves the text really reached
// this arm and lowered to the expected shape. Without the second half a
// collapsed parse would pass vacuously.
//
// Sourcing note: no PRINTED card pairs a player-target subject with an
// owner-scoped binding, a "under your control" binding, or a non-battlefield
// destination plus "under their control" (verified against the 35,516-card
// `data/card-data.json` export). These four are therefore deliberately
// synthetic CLASS probes built from the Beamtown grammar — the printed shapes
// they mimic ("under its owner's control", "under your control") are real, only
// the combination with a player-target subject is constructed. The fifth is a
// shipped card.
// ───────────────────────────────────────────────────────────────────────────

/// Drive a one-line sorcery through the real top-level parser entry point.
fn parse_sorcery(name: &str, text: &str) -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(text, name, &[], &["Sorcery".to_string()], &[])
}

/// Assert the parse is gap-free and return the chained `ChangeZone`'s
/// `(origin, destination, enters_under)` as the positive reach-guard.
///
/// Panics (rather than silently passing) when the text never lowered to a
/// `ChangeZone` at all — that is exactly the collapsed parse a "no gap node"
/// assertion alone would let through.
fn gap_free_change_zone(
    label: &str,
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> (Option<Zone>, Zone, Option<ControllerRef>) {
    assert!(
        parsed.parse_warnings.is_empty(),
        "{label}: the text must parse cleanly, got warnings {:?}",
        parsed.parse_warnings
    );
    assert_eq!(
        parsed.abilities.len(),
        1,
        "{label}: exactly one spell ability, got {:?}",
        parsed.abilities
    );
    let def = &parsed.abilities[0];
    assert_eq!(
        chain_unimplemented_names(def),
        Vec::<String>::new(),
        "{label}: the guard must NOT fire — no parse gap may be recorded, got {:?}",
        def.effect
    );

    let mut current = Some(def);
    while let Some(node) = current {
        if let Effect::ChangeZone {
            origin,
            destination,
            enters_under,
            ..
        } = node.effect.as_ref()
        {
            return (*origin, *destination, enters_under.clone());
        }
        current = node.sub_ability.as_deref();
    }
    panic!("{label}: reach-guard — the chain lowered no ChangeZone at all: {def:?}");
}

/// CR 110.2a: "under their owner's control" binds the OWNER, not the clause
/// subject, so it is a different (and separately deferred) gap — the
/// `enters_under_their_control` node must not fire on it.
///
/// This is the pin the phrase scan needs: `scan_contains` guarantees only a
/// LEADING word boundary, so widening the tag to "under their" (or dropping the
/// phrase conjunct entirely) makes this card export `supported: false` even
/// though it parsed correctly. Verified by mutation — see the report.
#[test]
fn enters_under_guard_silent_on_owner_scoped_plural_binding() {
    let parsed = parse_sorcery(
        "Guard Probe Their Owner",
        "Target opponent puts target nonlegendary creature card from your graveyard \
         onto the battlefield under their owner's control.",
    );
    let (origin, destination, enters_under) = gap_free_change_zone("owner-scoped plural", &parsed);
    // Reach-guard: the move really parsed, and `enters_under: None` is the
    // CORRECT lowering here — CR 110.2a's default for a battlefield entry is the
    // owner's control, which is precisely what the text asks for.
    assert_eq!(origin, Some(Zone::Graveyard));
    assert_eq!(destination, Zone::Battlefield);
    assert_eq!(
        enters_under, None,
        "owner-scoped text needs no override: None IS the owner default"
    );
}

/// CR 110.2a: the singular owner-scoped form ("under its owner's control", the
/// printed Brago / Displace / Eerie Interlude wording) is likewise not the
/// subject binding and must not be flagged.
#[test]
fn enters_under_guard_silent_on_owner_scoped_singular_binding() {
    let parsed = parse_sorcery(
        "Guard Probe Its Owner",
        "Target opponent puts target nonlegendary creature card from your graveyard \
         onto the battlefield under its owner's control.",
    );
    let (origin, destination, enters_under) =
        gap_free_change_zone("owner-scoped singular", &parsed);
    assert_eq!(origin, Some(Zone::Graveyard));
    assert_eq!(destination, Zone::Battlefield);
    assert_eq!(
        enters_under, None,
        "owner-scoped text needs no override: None IS the owner default"
    );
}

/// CR 110.2a: "under your control" IS carried today —
/// `enters_under: Some(ControllerRef::You)` — so there is no gap to record. A
/// guard that fired here would mark a fully-implemented binding unsupported.
#[test]
fn enters_under_guard_silent_on_bound_under_your_control() {
    let parsed = parse_sorcery(
        "Guard Probe Your Control",
        "Target opponent puts target nonlegendary creature card from your graveyard \
         onto the battlefield under your control.",
    );
    let (origin, destination, enters_under) = gap_free_change_zone("under your control", &parsed);
    assert_eq!(origin, Some(Zone::Graveyard));
    assert_eq!(destination, Zone::Battlefield);
    // Reach-guard AND the reason the guard stays silent: the binding is bound.
    assert_eq!(
        enters_under,
        Some(ControllerRef::You),
        "the \"under your control\" binding is carried, so nothing is missing"
    );
}

/// The `destination == Battlefield` conjunct. Same subject grammar, same
/// trailing "under their control" phrase, but the move does not put anything
/// onto the battlefield — CR 110.2a's controller rule is battlefield-only, so
/// there is no controller binding to lose and no gap to record.
///
/// Dropping the destination conjunct makes this fail. It is the only conjunct
/// besides the phrase scan that is reachable from card text today.
#[test]
fn enters_under_guard_silent_on_non_battlefield_destination() {
    let parsed = parse_sorcery(
        "Guard Probe Hand",
        "Target opponent puts target nonlegendary creature card from your graveyard \
         into their hand under their control.",
    );
    let (origin, destination, enters_under) =
        gap_free_change_zone("non-battlefield destination", &parsed);
    assert_eq!(origin, Some(Zone::Graveyard));
    assert_eq!(
        destination,
        Zone::Hand,
        "reach-guard: the move really is off-battlefield"
    );
    assert_eq!(enters_under, None);
}

/// Shipped-card non-regression for the same arm: Dwell on the Past, a real
/// "target player … from their graveyard into their library" move. Verified
/// Oracle text (MTGJSON export). No binding phrase, non-battlefield
/// destination — the arm must keep exporting it fully supported.
#[test]
fn enters_under_guard_silent_on_shipped_player_target_library_move() {
    let parsed = parse_sorcery(
        "Dwell on the Past",
        "Target player shuffles up to four target cards from their graveyard into their library.",
    );
    let (origin, destination, enters_under) = gap_free_change_zone("Dwell on the Past", &parsed);
    assert_eq!(origin, Some(Zone::Graveyard));
    assert_eq!(destination, Zone::Library);
    assert_eq!(enters_under, None);
}
