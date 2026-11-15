//! Viewer redaction of announcement-bound usage keys (CR 602.2a +
//! CR 602.5c + CR 400.2).
//!
//! The announced usage key and the activation record are engine authority:
//! they ride prompt-held carriers, paused mana abilities, the stack, and the
//! announcement journals, but no viewer projection may carry them. Three
//! carriers are covered: the active stack ability, a parked in-flight
//! activation (target prompt), and a paused mana ability (color-choice
//! prompt) — plus the per-priority guard and both journals. Trusted
//! save/restore round-trips the same key (retained, only redacted).
//!
//! Verbatim Oracle texts (Scryfall): Quirion Ranger — "Return a Forest you
//! control to its owner's hand: Untap target creature. Activate only once
//! each turn." Brigid, Doun's Mind — "{T}: Add X {G} or X {W}, where X is
//! the number of other creatures you control."
//!
//! Revert detectors: restoring any cleared field makes the corresponding
//! `is_none` assertion fail; restoring a journal/pending clear makes the
//! emptiness assertion fail.

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::move_to_zone;
use engine::types::ability::TargetRef;
use engine::types::ability::{AbilityDefinition, ContinuousModification, Effect};
use engine::types::ability_visit::{granter_symbols, DefinitionNode};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, ManaAbilityResume, ManaChoice, ManaChoiceContext, ManaChoicePrompt,
    PendingCast, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const BRIGID_TEXT: &str =
    "{T}: Add X {G} or X {W}, where X is the number of other creatures you control.";

/// A viewer with no seat at the table.
fn unseated() -> PlayerId {
    PlayerId(9)
}

/// Ranger board: one Untapper, one tappable target, one bounce Forest.
fn ranger_board(db: &engine::database::CardDatabase) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ranger = scenario.add_real_card(P0, "Quirion Ranger", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&bear)
        .expect("tappable")
        .tapped = true;
    (runner, ranger, bear, forest)
}

fn ranger_index(runner: &GameRunner, ranger: ObjectId) -> usize {
    runner.state().objects[&ranger]
        .abilities
        .iter()
        .position(|ability| {
            ability.kind == engine::types::ability::AbilityKind::Activated
                && ability
                    .description
                    .as_deref()
                    .is_some_and(|text| text.contains("Untap"))
        })
        .expect("the Ranger carries its printed Untap")
}

/// Announce the Ranger's activation and stop at its target prompt, with the
/// announcement-bound key and record riding the parked carrier.
fn announce_to_target_prompt(runner: &mut GameRunner, ranger: ObjectId) -> PendingCast {
    runner
        .act(GameAction::ActivateAbility {
            source_id: ranger,
            ability_index: ranger_index(runner, ranger),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announcement must be accepted");
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!(
            "expected the target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        pending_cast.ability.announced_usage_key.is_some(),
        "reach-guard: the announcement bound the key before targets"
    );
    assert!(
        pending_cast.ability.activation_record.is_some(),
        "reach-guard: the activation record rides the prompt"
    );
    pending_cast.as_ref().clone()
}

/// CR 400.2 + CR 602.2a + CR 602.5c: a parked in-flight activation and the
/// resolved stack entry project no announced key and no activation record,
/// and neither do the announcement journals or the per-priority guard — for
/// the opponent and for an unseated viewer alike.
#[test]
fn activation_prompt_and_stack_project_no_announced_key() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, ranger, bear, forest) = ranger_board(db);
    let viewers = [P1, unseated()];

    // Stage 1: the target prompt holds the parked carrier.
    let parked = announce_to_target_prompt(&mut runner, ranger);
    let parked_key = parked
        .ability
        .announced_usage_key
        .clone()
        .expect("the parked carrier is keyed");
    for viewer in viewers {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        let WaitingFor::TargetSelection { pending_cast, .. } = &filtered.waiting_for else {
            panic!(
                "{viewer:?} must see the same target prompt, got {:?}",
                filtered.waiting_for
            );
        };
        assert!(
            pending_cast.ability.announced_usage_key.is_none(),
            "{viewer:?} sees a parked announced key"
        );
        assert!(
            pending_cast.ability.activation_record.is_none(),
            "{viewer:?} sees a parked activation record"
        );
    }

    // Stage 2: answer target + bounce cost — the ability is placed with its
    // key, the journal counts it, and the per-priority guard names it.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("target must be accepted");
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("the bounce cost must be accepted");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the ability must be placed, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the announced ability must sit on the stack"
    );
    let StackEntryKind::ActivatedAbility { ability, .. } =
        &runner.state().stack.back().expect("placed").kind
    else {
        panic!("the stack top must be the announced activation");
    };
    assert_eq!(
        ability.announced_usage_key.as_ref(),
        Some(&parked_key),
        "reach-guard: placement publishes the announced key, never a re-derivation"
    );
    // The record is consumed by value into the journal at placement
    // (`record_activated_ability_placed` takes it), so the stack entry
    // carries the key but no record — while the parked prompt carrier above
    // still rode both.
    assert!(
        ability.activation_record.is_none(),
        "reach-guard: placement moved the record into the journal"
    );
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&parked_key)
            .copied(),
        Some(1),
        "reach-guard: the journal counts the announcement"
    );
    assert!(
        runner.state().pending_activations.contains(&parked_key),
        "reach-guard: the per-priority guard names the announcement"
    );

    // Trusted round-trip retains the key (redaction only redacts).
    let round_tripped: engine::types::game_state::GameState =
        serde_json::from_str(&serde_json::to_string(runner.state()).expect("serializes"))
            .expect("deserializes");
    let StackEntryKind::ActivatedAbility { ability, .. } =
        &round_tripped.stack.back().expect("placed").kind
    else {
        panic!("the round-tripped stack top must be the announced activation");
    };
    assert_eq!(
        ability.announced_usage_key.as_ref(),
        Some(&parked_key),
        "save/restore must retain the announced key"
    );

    for viewer in viewers {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        let StackEntryKind::ActivatedAbility { ability, .. } =
            &filtered.stack.back().expect("placed").kind
        else {
            panic!("{viewer:?} must see the stack entry");
        };
        assert!(
            ability.announced_usage_key.is_none(),
            "{viewer:?} sees a stack-resident announced key"
        );
        assert!(
            ability.activation_record.is_none(),
            "{viewer:?} sees a stack-resident activation record"
        );
        assert!(
            filtered.pending_activations.is_empty(),
            "{viewer:?} sees the per-priority guard"
        );
        assert!(
            filtered.activated_abilities_this_turn.is_empty(),
            "{viewer:?} sees the turn journal"
        );
        assert!(
            filtered.activated_abilities_this_game.is_empty(),
            "{viewer:?} sees the game journal"
        );
    }
}

/// CR 400.2 + CR 602.2a + CR 605.3b: a paused mana ability (Brigid's
/// color-choice prompt) projects no announced usage key — the choke point
/// clears it — while the authoritative state keeps it.
#[test]
fn pending_mana_pause_projects_no_announced_key() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brigid = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID_TEXT)
        .id();
    // Two other creatures the active player controls — X resolves to 2.
    // No pool funding: the {T} cost taps Brigid, and the assertions read the
    // exact produced count.
    scenario.add_creature(P0, "Other Creature A", 1, 1);
    scenario.add_creature(P0, "Other Creature B", 1, 1);
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: brigid,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Brigid's mana ability must succeed");
    let WaitingFor::ChooseManaColor {
        choice, context, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the color-choice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(choice, ManaChoicePrompt::SingleColor { .. }),
        "reach-guard: the AnyOneColor production offers a color choice"
    );
    let ManaChoiceContext::ManaAbility(pending) = context else {
        panic!("the color prompt must retain the paused mana ability");
    };
    assert!(
        pending.announced_usage_key.is_some(),
        "reach-guard: the mana announcement bound the key before the pause"
    );

    for viewer in [P1, unseated()] {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        let WaitingFor::ChooseManaColor { context, .. } = &filtered.waiting_for else {
            panic!(
                "{viewer:?} must see the same color prompt, got {:?}",
                filtered.waiting_for
            );
        };
        let ManaChoiceContext::ManaAbility(pending) = context else {
            panic!("{viewer:?} must see the paused mana ability");
        };
        assert!(
            pending.announced_usage_key.is_none(),
            "{viewer:?} sees a paused announced key"
        );
    }

    // The authoritative path completes undisturbed by the projection.
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("submitting the Green color choice must succeed");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        2,
        "X = 2 other creatures yields two green mana"
    );
}

// Verbatim Scryfall Oracle text (Marvin, Murderous Mimic).
const MARVIN: &str = "Marvin has all activated abilities of creatures you control that don't have the same name as this creature.";
// Verbatim Oracle text (Experiment Kraj), committed const in
// `cost_removed_counter_amount.rs`.
const KRAJ: &str = "Experiment Kraj has all activated abilities of each other creature with a +1/+1 counter on it.\n{T}: Put a +1/+1 counter on target creature.";
// Standard printed text (Elvish Mystic). DERIVED — no committed const exists.
const MYSTIC: &str = "{T}: Add {G}.";

fn relayer(state: &mut engine::types::game_state::GameState) {
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

/// The `{G}` mana donations on `recipient` (the re-donated lineage under test).
fn mana_donations(
    state: &engine::types::game_state::GameState,
    recipient: ObjectId,
) -> Vec<&engine::types::ability::AbilityDefinition> {
    state.objects[&recipient]
        .abilities
        .iter()
        .filter(|def| {
            def.description
                .as_deref()
                .is_some_and(|text| text.contains("Add {G}"))
        })
        .collect()
}

fn stamp_of(
    state: &engine::types::game_state::GameState,
    id: ObjectId,
) -> engine::types::identifiers::ObjectIncarnationRef {
    engine::types::identifiers::ObjectIncarnationRef::from_object(&state.objects[&id])
}

/// CR 400.2 + CR 613.1f: no projection discloses hidden provider lineage.
///
/// Board: Kraj gains the counter-bearing Mystic's `{G}` (first hop), Marvin
/// re-donates it (second hop: `granting_object` overwritten to Kraj, the
/// Mystic stamp pushed into `grant_chain`). The Mystic then moves to hand —
/// hidden from every viewer but its owner — while the layer pass is stale, so
/// authoritative state still holds the full chain naming the now-hidden id.
/// (The live pass would drop the grant on re-evaluation; the F1 parser pin
/// proves that separately.)
///
/// Revert detector: restoring any blanked stamp makes the projection scan
/// fail; dropping authoritative lineage fails the trusted-state assertion.
#[test]
fn projection_blanks_hidden_provider_lineage_but_keeps_public_hops() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marvin = scenario
        .add_creature_from_oracle(P0, "Marvin, Murderous Mimic", 2, 2, MARVIN)
        .id();
    let kraj = scenario
        .add_creature_from_oracle(P0, "Experiment Kraj", 4, 6, KRAJ)
        .id();
    let mystic = scenario
        .add_creature_from_oracle(P0, "Elvish Mystic", 1, 1, MYSTIC)
        .id();
    scenario.with_counter(mystic, engine::types::counter::CounterType::Plus1Plus1, 1);
    let mut runner = scenario.build();
    relayer(runner.state_mut());

    // Reach-guards: the two-hop lineage exists in authoritative state.
    // Marvin matches the battlefield Mystic twice: directly (its own filter
    // also names it) and through Kraj's re-donation — the same-body lineage
    // twins the F4 lookup must discriminate.
    let (kraj_stamp, mystic_stamp) = (
        stamp_of(runner.state(), kraj),
        stamp_of(runner.state(), mystic),
    );
    let kraj_mana = mana_donations(runner.state(), kraj);
    assert_eq!(
        kraj_mana.len(),
        1,
        "Kraj holds exactly the Mystic's donated mana ability; got {:?}",
        runner.state().objects[&kraj].abilities
    );
    assert_eq!(
        kraj_mana[0].granting_object,
        Some(mystic_stamp),
        "first hop stamps the Mystic"
    );
    assert!(
        kraj_mana[0].grant_chain.is_empty(),
        "the direct grant carries no chain"
    );
    let marvin_mana = mana_donations(runner.state(), marvin);
    assert_eq!(
        marvin_mana.len(),
        2,
        "Marvin holds the direct donation AND the re-donation; got {:?}",
        runner.state().objects[&marvin].abilities
    );
    let direct = marvin_mana
        .iter()
        .find(|def| def.grant_chain.is_empty())
        .expect("Marvin's direct Mystic donation");
    assert_eq!(
        direct.granting_object,
        Some(mystic_stamp),
        "the direct twin names the Mystic with no chain"
    );
    let redonated = marvin_mana
        .iter()
        .find(|def| !def.grant_chain.is_empty())
        .expect("Marvin's Kraj re-donation");
    assert_eq!(
        redonated.granting_object,
        Some(kraj_stamp),
        "re-donation overwrites the head stamp to the carrier"
    );
    assert_eq!(
        redonated.grant_chain,
        vec![mystic_stamp],
        "the overwritten first-hop stamp rides the chain"
    );

    // The Mystic moves to hand (hidden from P1 and the unseated viewer) while
    // the layer pass is stale, so authority keeps naming the now-hidden id.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), mystic, Zone::Hand, &mut events);
    assert_eq!(
        runner.state().objects[&mystic].zone,
        Zone::Hand,
        "reach-guard: the provider is hidden from the opponent"
    );

    for viewer in [P1, unseated()] {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        // No lineage entry anywhere in the projection may name the hidden id.
        for (id, object) in filtered.objects.iter() {
            for abilities in [&object.abilities, &object.base_abilities] {
                for def in abilities.iter() {
                    assert!(
                        def.granting_object
                            .is_none_or(|stamp| stamp.object_id != mystic),
                        "{viewer:?} sees a provider stamp naming the hidden Mystic on {id:?}"
                    );
                    assert!(
                        !def.grant_chain
                            .iter()
                            .any(|stamp| stamp.object_id == mystic),
                        "{viewer:?} sees a chain hop naming the hidden Mystic on {id:?}"
                    );
                }
            }
        }
        // The public hop survives on both twins: the direct twin's head stamp
        // (Mystic, hidden) is blanked while its empty chain stays empty; the
        // re-donated twin keeps naming Kraj while only the hidden hop leaves
        // the chain.
        let projected_mana = mana_donations(&filtered, marvin);
        assert_eq!(
            projected_mana.len(),
            2,
            "{viewer:?} still sees both donated mana abilities themselves"
        );
        let projected_direct = projected_mana
            .iter()
            .find(|def| def.granting_object.is_none() && def.grant_chain.is_empty())
            .expect("the direct twin, head-stamp blanked");
        assert!(
            projected_direct.grant_chain.is_empty(),
            "{viewer:?} keeps the direct twin's empty chain empty"
        );
        let projected_redonated = projected_mana
            .iter()
            .find(|def| def.granting_object == Some(kraj_stamp))
            .expect("the re-donated twin, carrier hop kept");
        assert!(
            projected_redonated.grant_chain.is_empty(),
            "{viewer:?} drops only the hidden hop, not the whole chain"
        );
    }

    // Authoritative state keeps the full chain (legitimate transitive budgets
    // survive — projection never feeds legality).
    let marvin_mana = mana_donations(runner.state(), marvin);
    let redonated = marvin_mana
        .iter()
        .find(|def| !def.grant_chain.is_empty())
        .expect("the authoritative re-donated twin");
    assert_eq!(
        redonated.grant_chain,
        vec![mystic_stamp],
        "authoritative state retains the hidden hop"
    );
    assert!(
        marvin_mana
            .iter()
            .any(|def| def.granting_object == Some(mystic_stamp) && def.grant_chain.is_empty()),
        "authoritative state retains the direct twin's head stamp"
    );
}

/// CR 400.2: the departed-incarnation tally tables are engine authority —
/// cleared wholesale in every projection, retained in trusted state.
///
/// The Ranger activates (live journal counts it), then leaves to the
/// graveyard (the zone exit migrates the entry to the departed table). Both
/// the opponent and the unseated viewer project empty tables while authority
/// retains the migrated count.
#[test]
fn projection_clears_departed_activation_tallies() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, ranger, bear, forest) = ranger_board(db);

    // Announce + place the Ranger's activation so the live journal counts it.
    announce_to_target_prompt(&mut runner, ranger);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("target must be accepted");
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("the bounce cost must be accepted");
    assert_eq!(
        runner.state().activated_abilities_this_turn.len(),
        1,
        "reach-guard: the live turn journal counts the announcement"
    );

    // The Ranger leaves to the graveyard: the live entry migrates.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), ranger, Zone::Graveyard, &mut events);
    assert!(
        runner.state().activated_abilities_this_turn.is_empty(),
        "reach-guard: the live journal cleared at the zone exit"
    );
    assert_eq!(
        runner.state().departed_activated_abilities_this_turn.len(),
        1,
        "reach-guard: the departed turn table holds the migrated entry"
    );

    for viewer in [P1, unseated()] {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        assert!(
            filtered.departed_activated_abilities_this_turn.is_empty(),
            "{viewer:?} sees the departed turn table"
        );
        assert!(
            filtered.departed_activated_abilities_this_game.is_empty(),
            "{viewer:?} sees the departed game table"
        );
    }

    // Trusted save/restore retains the departed tables (projection only clears).
    let round_tripped: engine::types::game_state::GameState =
        serde_json::from_str(&serde_json::to_string(runner.state()).expect("serializes"))
            .expect("deserializes");
    assert_eq!(
        round_tripped.departed_activated_abilities_this_turn.len(),
        1,
        "save/restore must retain the departed turn table"
    );
}

/// CR 400.2 + CR 613.1f: transient continuous effects project no hidden
/// provider lineage.
///
/// Board: Kraj gains the counter-bearing Mystic's `{G}` (stamped). Quicksilver
/// targets Kraj and resolves, snapshotting the stamped donation verbatim into
/// a transient `GrantAbility` clone. The Mystic then moves to hand — hidden
/// from every viewer but its owner — while the layer pass is stale. Opponent
/// and unseated projections must carry no transient modification or
/// effect-level stamp naming the hidden id, while authoritative state keeps
/// the clone intact (projection never feeds legality).
///
/// Revert detector: skipping the transient walk makes the projection scan
/// fail; dropping the authoritative clone fails the trusted-state assertion.
#[test]
fn projection_blanks_hidden_provider_lineage_in_transient_effects() {
    const QUICKSILVER: &str =
        "{U}: This creature gains all activated abilities of target creature until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kraj = scenario
        .add_creature_from_oracle(P0, "Experiment Kraj", 4, 6, KRAJ)
        .id();
    let mystic = scenario
        .add_creature_from_oracle(P0, "Elvish Mystic", 1, 1, MYSTIC)
        .id();
    let quicksilver = scenario
        .add_creature_from_oracle(P0, "Quicksilver Elemental", 2, 2, QUICKSILVER)
        .id();
    scenario.with_counter(mystic, engine::types::counter::CounterType::Plus1Plus1, 1);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    relayer(runner.state_mut());

    // Reach-guards: Kraj holds the stamped donation.
    let mystic_stamp = stamp_of(runner.state(), mystic);
    let kraj_mana = mana_donations(runner.state(), kraj);
    assert_eq!(
        kraj_mana.len(),
        1,
        "Kraj holds exactly the Mystic's donated mana ability"
    );
    assert_eq!(
        kraj_mana[0].granting_object,
        Some(mystic_stamp),
        "the live donation stamps the Mystic"
    );

    // Quicksilver snapshots Kraj's abilities into a transient effect.
    let gain = runner.state().objects[&quicksilver]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                *ability.effect,
                Effect::GainActivatedAbilitiesOfTarget { .. }
            )
        })
        .expect("Quicksilver's {U} ability");
    runner
        .activate(quicksilver, gain)
        .target_object(kraj)
        .resolve();
    runner.advance_until_stack_empty();
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .flat_map(|effect| effect.modifications.iter())
            .any(|modification| matches!(
                modification,
                ContinuousModification::GrantAbility { definition }
                    if definition.granting_object == Some(mystic_stamp)
            )),
        "reach-guard: the snapshot cloned the stamped donation verbatim"
    );

    // The Mystic moves to hand (hidden) while the layer pass is stale.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), mystic, Zone::Hand, &mut events);
    assert_eq!(
        runner.state().objects[&mystic].zone,
        Zone::Hand,
        "reach-guard: the provider is hidden from the opponent"
    );

    for viewer in [P1, unseated()] {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        assert!(
            !filtered.transient_continuous_effects.is_empty(),
            "{viewer:?} still sees the transient effect itself"
        );
        for effect in filtered.transient_continuous_effects.iter() {
            assert!(
                effect
                    .granting_object
                    .is_none_or(|stamp| stamp.object_id != mystic),
                "{viewer:?} sees an effect stamp naming the hidden Mystic"
            );
            for modification in effect.modifications.iter() {
                let heads = match modification {
                    ContinuousModification::GrantAbility { definition } => {
                        let mut heads = vec![definition.granting_object];
                        heads.extend(definition.grant_chain.iter().copied().map(Some));
                        heads
                    }
                    ContinuousModification::GrantTrigger { trigger } => {
                        vec![trigger.granting_object]
                    }
                    ContinuousModification::GrantStaticAbility { definition } => {
                        vec![definition.granting_object]
                    }
                    ContinuousModification::GrantReplacement { replacement } => {
                        vec![replacement.granting_object]
                    }
                    _ => vec![],
                };
                assert!(
                    !heads
                        .into_iter()
                        .flatten()
                        .any(|stamp| stamp.object_id == mystic),
                    "{viewer:?} sees a transient clone naming the hidden Mystic"
                );
            }
        }
    }

    // Authoritative state keeps the stamped clone (projection never feeds
    // legality).
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .flat_map(|effect| effect.modifications.iter())
            .any(|modification| matches!(
                modification,
                ContinuousModification::GrantAbility { definition }
                    if definition.granting_object == Some(mystic_stamp)
            )),
        "authoritative state retains the stamped transient clone"
    );
}

/// Every lineage stamp on a definition snapshot's node axis naming
/// `provider` — the same writer's-own axis the projection scrub walks.
fn snapshot_lineage_hits(
    snapshot: &AbilityDefinition,
    provider: ObjectId,
) -> Vec<ObjectIncarnationRef> {
    let mut hits = Vec::new();
    granter_symbols::each_node(DefinitionNode::Ability(snapshot), &mut |node| {
        let stamps = match node {
            DefinitionNode::Ability(def) => {
                let mut stamps = vec![def.granting_object];
                stamps.extend(def.grant_chain.iter().copied().map(Some));
                stamps
            }
            DefinitionNode::Trigger(def) => vec![def.granting_object],
            DefinitionNode::Static(def) => vec![def.granting_object],
            DefinitionNode::Replacement(def) => vec![def.granting_object],
        };
        hits.extend(
            stamps
                .into_iter()
                .flatten()
                .filter(|stamp| stamp.object_id == provider),
        );
    });
    hits
}

/// CR 400.2 + CR 613.1f + CR 605.3b: a paused granted mana ability projects
/// no hidden provider lineage in its activation-time snapshot.
///
/// Board: Kraj gains the counter-bearing Brigid's X-mana ability (a stamped
/// live donation); Quicksilver snapshots Kraj's stamped ability into a
/// transient grant. Brigid then moves to hand — hidden from every viewer but
/// its owner — while the duration-bound grant stays live. Activating
/// Quicksilver's granted copy pauses at the color prompt with a
/// `PendingManaAbility` whose `ability_snapshot` clones the stamped
/// definition. Opponent and unseated projections must carry no snapshot
/// lineage naming the hidden id (a node-axis scan plus an independent
/// serialized scan), while authoritative state and trusted save/restore keep
/// the clone intact. Answering Green completes the activation (X = 3).
///
/// Revert detector: skipping the snapshot walk makes both projection scans
/// fail; dropping the authoritative clone fails the trusted-state assertion.
#[test]
fn paused_granted_mana_snapshot_projects_no_hidden_lineage() {
    const QUICKSILVER_GRANT: &str =
        "{U}: This creature gains all activated abilities of target creature until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kraj = scenario
        .add_creature_from_oracle(P0, "Experiment Kraj", 4, 6, KRAJ)
        .id();
    let brigid = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID_TEXT)
        .id();
    let quicksilver = scenario
        .add_creature_from_oracle(P0, "Quicksilver Elemental", 2, 2, QUICKSILVER_GRANT)
        .id();
    scenario.add_creature(P0, "Other Creature A", 1, 1);
    scenario.add_creature(P0, "Other Creature B", 1, 1);
    scenario.with_counter(brigid, engine::types::counter::CounterType::Plus1Plus1, 1);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    relayer(runner.state_mut());

    // Reach-guards: Kraj holds the stamped Brigid donation.
    let brigid_stamp = stamp_of(runner.state(), brigid);
    fn is_brigid_mana(def: &AbilityDefinition) -> bool {
        def.description
            .as_deref()
            .is_some_and(|text| text.contains("Add X"))
    }
    let kraj_mana: Vec<&AbilityDefinition> = runner.state().objects[&kraj]
        .abilities
        .iter()
        .filter(|def| is_brigid_mana(def))
        .collect();
    assert_eq!(
        kraj_mana.len(),
        1,
        "Kraj holds exactly Brigid's donated mana ability"
    );
    assert_eq!(
        kraj_mana[0].granting_object,
        Some(brigid_stamp),
        "the live donation stamps Brigid"
    );

    // Quicksilver snapshots Kraj's stamped ability into a transient effect.
    let gain = runner.state().objects[&quicksilver]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                *ability.effect,
                Effect::GainActivatedAbilitiesOfTarget { .. }
            )
        })
        .expect("Quicksilver's {U} ability");
    runner
        .activate(quicksilver, gain)
        .target_object(kraj)
        .resolve();
    runner.advance_until_stack_empty();
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .flat_map(|effect| effect.modifications.iter())
            .any(|modification| matches!(
                modification,
                ContinuousModification::GrantAbility { definition }
                    if definition.granting_object == Some(brigid_stamp)
            )),
        "reach-guard: the snapshot cloned the stamped donation verbatim"
    );

    // Brigid moves to hand (hidden) while the duration-bound grant stays live.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), brigid, Zone::Hand, &mut events);
    assert_eq!(
        runner.state().objects[&brigid].zone,
        Zone::Hand,
        "reach-guard: the provider is hidden from the opponent"
    );

    // Activate Quicksilver's granted copy; it pauses at the color prompt.
    let granted = runner.state().objects[&quicksilver]
        .abilities
        .iter()
        .position(is_brigid_mana)
        .expect("Quicksilver's granted Brigid ability");
    let live = &runner.state().objects[&quicksilver].abilities[granted];
    assert!(
        live.granting_object == Some(brigid_stamp) || live.grant_chain.contains(&brigid_stamp),
        "reach-guard: the live granted copy names Brigid in its head stamp or chain"
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: quicksilver,
            ability_index: granted,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating the granted mana ability must succeed");
    let WaitingFor::ChooseManaColor {
        choice, context, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the color-choice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(choice, ManaChoicePrompt::SingleColor { .. }),
        "reach-guard: the AnyOneColor production offers a color choice"
    );
    let ManaChoiceContext::ManaAbility(pending) = context else {
        panic!("the color prompt must retain the paused mana ability");
    };
    let snapshot = pending
        .ability_snapshot
        .as_ref()
        .expect("reach-guard: the pause snapshots the activated definition");
    assert!(
        !snapshot_lineage_hits(snapshot, brigid).is_empty(),
        "reach-guard: the authoritative snapshot names the hidden provider"
    );
    let marker = format!("\"object_id\":{}", brigid.0);
    let raw = serde_json::to_string(snapshot).expect("the snapshot serializes");
    assert!(
        raw.contains(&marker),
        "reach-guard: the serialized authoritative snapshot names the hidden id"
    );

    for viewer in [P1, unseated()] {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        let WaitingFor::ChooseManaColor { context, .. } = &filtered.waiting_for else {
            panic!(
                "{viewer:?} must see the same color prompt, got {:?}",
                filtered.waiting_for
            );
        };
        let ManaChoiceContext::ManaAbility(pending) = context else {
            panic!("{viewer:?} must see the paused mana ability");
        };
        let snapshot = pending
            .ability_snapshot
            .as_ref()
            .expect("the viewer still sees the snapshot itself");
        assert!(
            snapshot_lineage_hits(snapshot, brigid).is_empty(),
            "{viewer:?} sees snapshot lineage naming the hidden provider"
        );
        let projected = serde_json::to_string(snapshot).expect("serializes");
        assert!(
            !projected.contains(&marker),
            "{viewer:?} sees the hidden provider id anywhere in the snapshot"
        );
    }

    // Trusted save/restore retains the stamped snapshot (projection only redacts).
    let round_tripped: engine::types::game_state::GameState =
        serde_json::from_str(&serde_json::to_string(runner.state()).expect("serializes"))
            .expect("deserializes");
    let WaitingFor::ChooseManaColor { context, .. } = &round_tripped.waiting_for else {
        panic!(
            "the round-tripped halt must be the color prompt, got {:?}",
            round_tripped.waiting_for
        );
    };
    let ManaChoiceContext::ManaAbility(pending) = context else {
        panic!("the round-tripped prompt must retain the paused mana ability");
    };
    assert!(
        !snapshot_lineage_hits(
            pending
                .ability_snapshot
                .as_ref()
                .expect("save/restore must retain the snapshot"),
            brigid
        )
        .is_empty(),
        "save/restore must retain the snapshot's hidden lineage"
    );

    // The authoritative path completes undisturbed by the projection.
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("submitting the Green color choice must succeed");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        3,
        "X = 3 other creatures yields three green mana"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must complete, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the inline activation"
    );
}

/// CR 400.2 + CR 605.3a + CR 605.3b + CR 613.1f: nested paused mana
/// abilities project no hidden provider lineage in EITHER snapshot.
///
/// Board: Kraj gains the counter-bearing Brigid's X-mana ability AND the
/// counter-bearing Skyshroud Elf's abilities; two Quicksilvers snapshot
/// Kraj's stamped donations into transient grants. Both donors move to hand
/// (hidden) while the duration-bound grants stay live. Activating the first
/// Quicksilver's granted "{1}: Add {R} or {W}" with an empty pool opens the
/// payment window; activating the second Quicksilver's granted Brigid
/// ability inside that window pauses at the color prompt with the outer
/// pending nested in the resume chain (`ManaAbilityManaPayment`). Opponent
/// and unseated projections must carry neither hidden id anywhere in the
/// prompt carrier (a whole-carrier serialized scan plus per-snapshot
/// node-axis scans), while authority keeps both clones and the stack
/// completes (inner Green pays the outer {1}, outer answers Red).
///
/// Revert detector: skipping the snapshot walk makes the projection scans
/// fail; dropping an authoritative clone fails the trusted-state assertions.
#[test]
fn nested_paused_granted_mana_snapshots_project_no_hidden_lineage() {
    const QUICKSILVER_NESTED: &str =
        "{U}: This creature gains all activated abilities of target creature until end of turn.";
    let Some(db) = shared_card_db() else { return };
    fn is_brigid_mana(def: &AbilityDefinition) -> bool {
        def.description
            .as_deref()
            .is_some_and(|text| text.contains("Add X"))
    }
    fn is_elf_costed_mana(def: &AbilityDefinition) -> bool {
        def.description
            .as_deref()
            .is_some_and(|text| text.contains("Add {R} or {W}"))
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kraj = scenario
        .add_creature_from_oracle(P0, "Experiment Kraj", 4, 6, KRAJ)
        .id();
    let brigid = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID_TEXT)
        .id();
    let elf = scenario.add_real_card(P0, "Skyshroud Elf", Zone::Battlefield, db);
    let qa = scenario
        .add_creature_from_oracle(P0, "Quicksilver Elemental", 2, 2, QUICKSILVER_NESTED)
        .id();
    let qb = scenario
        .add_creature_from_oracle(P0, "Quicksilver Elemental", 2, 2, QUICKSILVER_NESTED)
        .id();
    scenario.add_creature(P0, "Other Creature A", 1, 1);
    scenario.add_creature(P0, "Other Creature B", 1, 1);
    scenario.with_counter(brigid, engine::types::counter::CounterType::Plus1Plus1, 1);
    scenario.with_counter(elf, engine::types::counter::CounterType::Plus1Plus1, 1);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    relayer(runner.state_mut());

    // Reach-guards: Kraj holds both stamped donations.
    let brigid_stamp = stamp_of(runner.state(), brigid);
    let elf_stamp = stamp_of(runner.state(), elf);
    let kraj_brigid: Vec<&AbilityDefinition> = runner.state().objects[&kraj]
        .abilities
        .iter()
        .filter(|def| is_brigid_mana(def))
        .collect();
    assert_eq!(
        kraj_brigid.len(),
        1,
        "Kraj holds exactly Brigid's donated mana ability"
    );
    assert_eq!(
        kraj_brigid[0].granting_object,
        Some(brigid_stamp),
        "the live Brigid donation stamps Brigid"
    );
    let kraj_elf: Vec<&AbilityDefinition> = runner.state().objects[&kraj]
        .abilities
        .iter()
        .filter(|def| is_elf_costed_mana(def))
        .collect();
    assert_eq!(
        kraj_elf.len(),
        1,
        "Kraj holds exactly the Elf's costed mana ability"
    );
    assert_eq!(
        kraj_elf[0].granting_object,
        Some(elf_stamp),
        "the live Elf donation stamps the Elf"
    );

    // Both Quicksilvers snapshot Kraj's stamped abilities.
    for q in [qa, qb] {
        let gain = runner.state().objects[&q]
            .abilities
            .iter()
            .position(|ability| {
                matches!(
                    *ability.effect,
                    Effect::GainActivatedAbilitiesOfTarget { .. }
                )
            })
            .expect("Quicksilver's {U} ability");
        runner.activate(q, gain).target_object(kraj).resolve();
        runner.advance_until_stack_empty();
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "reach-guard: both {{U}} payments left the pool empty, so the outer {{1}} opens a window"
    );

    // Both donors move to hand (hidden) while the duration-bound grants live.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), brigid, Zone::Hand, &mut events);
    move_to_zone(runner.state_mut(), elf, Zone::Hand, &mut events);
    assert_eq!(
        runner.state().objects[&brigid].zone,
        Zone::Hand,
        "reach-guard: Brigid is hidden from the opponent"
    );
    assert_eq!(
        runner.state().objects[&elf].zone,
        Zone::Hand,
        "reach-guard: the Elf is hidden from the opponent"
    );

    // The outer (costed) and inner (pausing) granted copies name their hidden
    // providers in the head stamp or the chain.
    let outer_idx = runner.state().objects[&qa]
        .abilities
        .iter()
        .position(is_elf_costed_mana)
        .expect("QA's granted Elf ability");
    let outer_live = &runner.state().objects[&qa].abilities[outer_idx];
    assert!(
        outer_live.granting_object == Some(elf_stamp)
            || outer_live.grant_chain.contains(&elf_stamp),
        "reach-guard: the live outer copy names the Elf"
    );
    let inner_idx = runner.state().objects[&qb]
        .abilities
        .iter()
        .position(is_brigid_mana)
        .expect("QB's granted Brigid ability");
    let inner_live = &runner.state().objects[&qb].abilities[inner_idx];
    assert!(
        inner_live.granting_object == Some(brigid_stamp)
            || inner_live.grant_chain.contains(&brigid_stamp),
        "reach-guard: the live inner copy names Brigid"
    );

    // Open the outer payment window (Manual: no auto-tap covers the {1}),
    // then activate the inner ability inside it.
    runner
        .act(GameAction::ActivateAbility {
            source_id: qa,
            ability_index: outer_idx,
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("activating the granted costed ability must succeed");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaAbilityManaPayment { .. } | WaitingFor::ManaPayment { .. }
        ),
        "reach-guard: the unpaid outer {{1}} opens a payment window, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: qb,
            ability_index: inner_idx,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating the inner ability inside the window must succeed");
    let WaitingFor::ChooseManaColor {
        choice, context, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the inner color-choice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(choice, ManaChoicePrompt::SingleColor { .. }),
        "reach-guard: the inner AnyOneColor production offers a color choice"
    );
    let ManaChoiceContext::ManaAbility(inner) = context else {
        panic!("the color prompt must retain the paused inner ability");
    };
    // The nesting reach-guard: the outer pending rides the inner resume.
    let ManaAbilityResume::ManaAbilityManaPayment {
        pending_mana_ability: outer,
    } = &inner.resume
    else {
        panic!(
            "the inner resume must nest the outer pending, got {:?}",
            inner.resume
        );
    };
    let inner_snapshot = inner
        .ability_snapshot
        .as_ref()
        .expect("reach-guard: the inner pause snapshots its definition");
    let outer_snapshot = outer
        .ability_snapshot
        .as_ref()
        .expect("reach-guard: the nested outer pause snapshots its definition");
    assert!(
        !snapshot_lineage_hits(inner_snapshot, brigid).is_empty(),
        "reach-guard: the authoritative inner snapshot names Brigid"
    );
    assert!(
        !snapshot_lineage_hits(outer_snapshot, elf).is_empty(),
        "reach-guard: the authoritative outer snapshot names the Elf"
    );
    // Whole-carrier serialized reach-guards: both hidden ids ride the prompt.
    let raw = serde_json::to_string(&runner.state().waiting_for).expect("serializes");
    for (label, id) in [("Brigid", brigid), ("Elf", elf)] {
        let marker = format!("\"object_id\":{}", id.0);
        assert!(
            raw.contains(&format!("{marker},")) || raw.contains(&format!("{marker}}}")),
            "reach-guard: the serialized authoritative carrier names hidden {label}"
        );
    }

    for viewer in [P1, unseated()] {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        let WaitingFor::ChooseManaColor { context, .. } = &filtered.waiting_for else {
            panic!(
                "{viewer:?} must see the same color prompt, got {:?}",
                filtered.waiting_for
            );
        };
        let ManaChoiceContext::ManaAbility(inner) = context else {
            panic!("{viewer:?} must see the paused inner ability");
        };
        let ManaAbilityResume::ManaAbilityManaPayment {
            pending_mana_ability: outer,
        } = &inner.resume
        else {
            panic!("{viewer:?} must see the nested outer pending");
        };
        for (label, snapshot, hidden) in [
            ("inner", &inner.ability_snapshot, brigid),
            ("outer", &outer.ability_snapshot, elf),
        ] {
            let snapshot = snapshot
                .as_ref()
                .expect("the viewer still sees the snapshot itself");
            assert!(
                snapshot_lineage_hits(snapshot, hidden).is_empty(),
                "{viewer:?} sees {label} snapshot lineage naming its hidden provider"
            );
        }
        // The whole projected carrier names neither hidden id anywhere.
        let projected = serde_json::to_string(&filtered.waiting_for).expect("serializes");
        for (label, id) in [("Brigid", brigid), ("Elf", elf)] {
            let marker = format!("\"object_id\":{}", id.0);
            assert!(
                !projected.contains(&format!("{marker},"))
                    && !projected.contains(&format!("{marker}}}")),
                "{viewer:?} sees hidden {label} anywhere in the prompt carrier"
            );
        }
    }

    // Trusted save/restore retains both stamped snapshots.
    let round_tripped: engine::types::game_state::GameState =
        serde_json::from_str(&serde_json::to_string(runner.state()).expect("serializes"))
            .expect("deserializes");
    let WaitingFor::ChooseManaColor { context, .. } = &round_tripped.waiting_for else {
        panic!(
            "the round-tripped halt must be the color prompt, got {:?}",
            round_tripped.waiting_for
        );
    };
    let ManaChoiceContext::ManaAbility(inner) = context else {
        panic!("the round-tripped prompt must retain the paused inner ability");
    };
    assert!(
        !snapshot_lineage_hits(
            inner
                .ability_snapshot
                .as_ref()
                .expect("save/restore must retain the inner snapshot"),
            brigid
        )
        .is_empty(),
        "save/restore must retain the inner snapshot's hidden lineage"
    );

    // Complete the inner prompt; the outer window resumes and its {1} pays
    // from the produced mana, then the outer production choice follows.
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("submitting the Green color choice must succeed");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaAbilityManaPayment { .. } | WaitingFor::PayManaAbilityMana { .. }
        ),
        "the outer window must resume after the inner completes, got {:?}",
        runner.state().waiting_for
    );
    // No more activations to make: passing priority spends the floating pool
    // mana into the outer {1} and the outer production choice follows.
    runner
        .act(GameAction::PassPriority)
        .expect("passing priority must spend the pool into the outer cost");
    let WaitingFor::ChooseManaColor {
        choice, context, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the outer production choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(choice, ManaChoicePrompt::SingleColor { .. }),
        "the outer Add {{R}} or {{W}} offers a color choice"
    );
    let ManaChoiceContext::ManaAbility(_) = context else {
        panic!("the outer prompt must retain the paused outer ability");
    };
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        })
        .expect("submitting the Red color choice must succeed");
    let pool = &runner.state().players[P0.0 as usize].mana_pool;
    assert_eq!(
        pool.count_color(ManaType::Green),
        3,
        "X = 4 Green minus the outer {{1}} leaves three Green"
    );
    assert_eq!(
        pool.count_color(ManaType::Red),
        1,
        "the outer production yields one Red"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "both activations must complete, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after the inline activations"
    );
}
