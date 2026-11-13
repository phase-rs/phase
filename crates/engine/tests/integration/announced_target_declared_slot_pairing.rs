//! CR 601.2c: the announced-target latch is ONE statement about ONE
//! announcement, so each recorded fact belongs to the DECLARED SLOT that
//! produced it — not to "the first declared slot that names the same object".
//!
//! "If the spell uses the word 'target' in multiple places, the same object or
//! player can be chosen once for each instance of the word 'target' (as long as
//! it fits the targeting criteria)." One object may therefore fill two slots
//! with two DIFFERENT requirements. Kolaghan's Command's "Destroy target
//! artifact" and "Kolaghan's Command deals 2 damage to any target" can both be
//! announced at the same artifact creature. A guard that looks a fact up by
//! first-match on its object id judges BOTH facts against the artifact slot's
//! requirement and never checks the damage slot at all — so the damage slot's
//! requirement is unchecked, and a latch that lost or rewrote the damage fact
//! is admitted.
//!
//! `GameState::reject_contradictory_announced_target_facts` pairs them
//! positionally now: the latch is `declared_targets_in_chain` projected through
//! `capture_target_facts`, which visits the declared slots in numbering order
//! and drops only a target it cannot snapshot, so the n-th fact is the n-th
//! SURVIVING slot, and each pair supplies its own filter and its own
//! announcement pin.
//!
//! The `repeated_target_*` witnesses below exercise the pairing through a
//! fixture whose announcement is the two clauses themselves; the
//! `modal_command_*` witnesses exercise it through the REAL modal route:
//! CR 601.2b's mode announcement is answered at `WaitingFor::ModeChoice` with
//! `GameAction::SelectModes`, and the two declared slots the pairing walks are
//! the two chosen modes.
//!
//! The second rule here is CR 405.1: a target on the STACK is a real target.
//! A Counterspell announced at a spell carries a fact whose captured zone is
//! `Zone::Stack`, and the recorded-announcement matcher used to answer the
//! `StackSpell` leaf of that slot's filter with a definite FALSE — refusing an
//! untouched save taken during mana payment. The record cannot name a stack
//! entry's kind (a spell and an ability share the zone), so the honest answer is
//! UNKNOWN: an unanswerable predicate must not read as a failed one.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ModalChoice, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    ActivationTargetFact, AnnouncedTargetFactConflict, AnnouncedTargetZone, CapturedTokenBit,
    CastPaymentMode, PersistedGameState, PersistedRestoreError, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall / MTGJSON): Kolaghan's Command, an instant.
/// The four modes are in the card's PRINTED order — return, discard, destroy,
/// damage — so a mode index in this file is the printed mode index.
const KOLAGHANS_COMMAND_ORACLE: &str = "Choose two —\n\
     • Return target creature card from your graveyard to your hand.\n\
     • Target player discards a card.\n\
     • Destroy target artifact.\n\
     • Kolaghan's Command deals 2 damage to any target.";

/// CR 601.2c: the two clauses a caster who chose "Destroy target artifact" and
/// "… deals 2 damage to any target" announced. An artifact CREATURE fits both
/// ("as long as it fits the targeting criteria"), so the same object fills both
/// slots with two different requirements. The `repeated_target_*` fixture parses
/// this pair of clauses directly (the duplicate-slot control); the
/// `modal_command_*` witnesses below reach the same two slots through the
/// printed modal block, answering its mode choice as a player does.
const TWO_CHOSEN_MODES: &str =
    "Destroy target artifact.\nKolaghan's Command deals 2 damage to any target.";

/// Verbatim Oracle text (Scryfall): Counterspell, an instant.
const COUNTERSPELL_ORACLE: &str = "Counter target spell.";

fn red() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    }
}

fn blue_blue() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    }
}

/// CR 601.2c: the target a recorded fact names.
fn fact_target_ref(fact: &ActivationTargetFact) -> TargetRef {
    match fact {
        ActivationTargetFact::Object { id, .. } => TargetRef::Object(*id),
        ActivationTargetFact::Player(player) => TargetRef::Player(*player),
    }
}

/// Save the parked cast through the authoritative capture and read it back.
fn save_and_restore(
    runner: &GameRunner,
) -> Result<engine::types::game_state::GameState, PersistedRestoreError> {
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the parked cast serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the parked cast deserializes");
    restored.into_game_state()
}

// ---------------------------------------------------------------------------
// BLOCKER 1 — the same object in two slots with two different requirements.
// ---------------------------------------------------------------------------

struct ParkedCommand {
    runner: GameRunner,
    spell: ObjectId,
    artifact_creature: ObjectId,
    mountain: ObjectId,
}

/// CR 601.2c: announce Kolaghan's Command's artifact-destruction and damage
/// modes, both at the SAME artifact creature, with `CastPaymentMode::Manual` so
/// the cast parks at `WaitingFor::ManaPayment` holding its announced-target
/// latch — the state a save at the payment boundary captures.
fn park_command_at_manual_payment() -> ParkedCommand {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact_creature = scenario
        .add_creature(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kolaghan's Command", true, TWO_CHOSEN_MODES)
        // The engine parses the card's text; the two clauses below are the
        // announcement under test.
        .with_mana_cost(red())
        .id();
    assert!(
        KOLAGHANS_COMMAND_ORACLE.contains("Destroy target artifact")
            && KOLAGHANS_COMMAND_ORACLE.contains("deals 2 damage to any target")
            && TWO_CHOSEN_MODES
                .lines()
                .all(|clause| KOLAGHANS_COMMAND_ORACLE
                    .contains(clause.trim_start_matches('•').trim())),
        "reach-guard: both parsed clauses are verbatim clauses of this card"
    );
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the manual Kolaghan's Command cast is announced");
    // CR 601.2c: answer each declared slot with the SAME object — legal because
    // the spell uses the word "target" in two places.
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(artifact_creature)),
                    })
                    .expect("CR 601.2c: the same object fits both slots' requirements");
            }
            WaitingFor::ManaPayment { player, .. } => {
                assert_eq!(player, P0, "the caster pays");
                break;
            }
            other => panic!("unexpected prompt while announcing the two modes: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "reach-guard: the cast parks at manual payment, got {:?}",
        runner.state().waiting_for
    );
    ParkedCommand {
        runner,
        spell,
        artifact_creature,
        mountain,
    }
}

/// The parked cast's pending `PendingCast`, which carries both the declaration
/// (the ability chain) and the latch the guard binds to it.
fn parked_pending(runner: &GameRunner) -> &engine::types::game_state::PendingCast {
    runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach-guard: the manual payment park keeps its pending cast")
}

/// Reach guards shared by every witness below: the cast declared TWO slots with
/// DIFFERENT requirements, the latch holds one fact per slot, and both facts
/// name the SAME object.
fn assert_repeated_target_reach_guards(parked: &ParkedCommand) {
    let pending = parked_pending(&parked.runner);
    let root = pending.ability.as_ref();
    let second = root
        .sub_ability
        .as_deref()
        .expect("reach-guard: the two chosen modes are a two-instruction chain");
    let first_filter = root
        .effect
        .target_filter()
        .expect("reach-guard: the artifact-destruction clause declares a target");
    let second_filter = second
        .effect
        .target_filter()
        .expect("reach-guard: the damage clause declares a target");
    assert_ne!(
        first_filter, second_filter,
        "reach-guard: the two slots carry DIFFERENT requirements — which is what makes the \
         identity-first-match lookup judge the damage fact against the artifact requirement"
    );

    let facts = pending
        .committed_target_facts
        .as_deref()
        .expect("reach-guard: the announcement latch is populated at the payment park");
    assert_eq!(
        facts.len(),
        2,
        "CR 601.2c: two declared slots owe two facts, got {facts:?}"
    );
    for (index, fact) in facts.iter().enumerate() {
        match fact {
            ActivationTargetFact::Object {
                id, zone, token, ..
            } => {
                assert_eq!(
                    *id, parked.artifact_creature,
                    "reach-guard: fact {index} names the one object both slots chose"
                );
                assert_eq!(
                    *zone,
                    AnnouncedTargetZone::Known(Zone::Battlefield),
                    "reach-guard: the artifact creature was announced on the battlefield"
                );
                assert_eq!(
                    *token,
                    CapturedTokenBit::Known(false),
                    "reach-guard: the recorded token bit says the target was no token"
                );
            }
            other => panic!("reach-guard: fact {index} is an object fact, got {other:?}"),
        }
    }
    assert!(
        parked.runner.state().objects[&parked.artifact_creature]
            .card_types
            .core_types
            .contains(&CoreType::Artifact)
            && parked.runner.state().objects[&parked.artifact_creature]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
        "reach-guard: the announced object is an artifact creature — legal for both slots"
    );
}

/// WITNESS (BLOCKER 1, honest direction): a save whose latch records the same
/// object once per slot, each satisfying ITS OWN slot's requirement, restores
/// and resumes.
#[test]
fn repeated_target_save_restores_and_resumes() {
    let parked = park_command_at_manual_payment();
    assert_repeated_target_reach_guards(&parked);

    let restored = save_and_restore(&parked.runner)
        .expect("CR 601.2c: an honest repeated-target latch is admitted");
    let mut runner = GameRunner::from_state(restored);

    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&parked.mountain)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the restored payment prompt offers the Mountain's own {T}: Add {R}");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("the restored cast takes the mana it announced it would");
    runner
        .act(GameAction::PassPriority)
        .expect("the restored manual payment finalizes");
    assert_eq!(
        runner.state().objects[&parked.spell].zone,
        Zone::Stack,
        "reach-guard: the resumed cast reached the stack"
    );

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the spell resolved");
    assert_eq!(
        runner.state().objects[&parked.artifact_creature].zone,
        Zone::Graveyard,
        "CR 701.8a: the resumed cast destroys the artifact it announced"
    );
    let recorded_targets: Option<Vec<TargetRef>> = runner
        .state()
        .spells_cast_this_turn_by_player
        .values()
        .flat_map(|records| records.iter())
        .find(|record| record.spell_object_id == Some(parked.spell))
        .and_then(|record| record.targets.as_deref())
        .map(|facts| facts.iter().map(fact_target_ref).collect());
    assert_eq!(
        recorded_targets,
        Some(vec![
            TargetRef::Object(parked.artifact_creature),
            TargetRef::Object(parked.artifact_creature),
        ]),
        "CR 601.2i: the resumed cast publishes BOTH announced occurrences"
    );
}

/// WITNESS (BLOCKER 1, refusal direction A): the same save with the SECOND
/// fact deleted is a latch that records one occurrence where the spell declared
/// two. The object is still on the battlefield, so the honest capture would have
/// produced that fact; its absence must be refused rather than resumed into a
/// history row that lost an announced target.
#[test]
fn repeated_target_save_missing_its_second_fact_is_refused() {
    let parked = park_command_at_manual_payment();
    assert_repeated_target_reach_guards(&parked);

    let mut state = parked.runner.state().clone();
    {
        let pending = state
            .pending_cast
            .as_mut()
            .expect("reach-guard: the manual payment park keeps its pending cast");
        let facts = pending
            .committed_target_facts
            .as_mut()
            .expect("reach-guard: the announcement latch is populated");
        // The SECOND occurrence is the one the damage slot produced.
        facts.truncate(1);
    }

    let error = PersistedGameState::capture(state)
        .into_game_state()
        .expect_err("CR 601.2c: a latch that lost one of two announced occurrences is refused");
    match error {
        PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason,
        } => {
            assert_eq!(spell_object_id, parked.spell, "the refusal names the cast");
            match reason {
                AnnouncedTargetFactConflict::OmittedDeclaredTarget { target } => assert_eq!(
                    target,
                    TargetRef::Object(parked.artifact_creature),
                    "and the declared target whose occurrence was deleted"
                ),
                other => panic!("expected the omitted-occurrence refusal, got {other:?}"),
            }
        }
        other => panic!("expected a declared-target refusal, got {other:?}"),
    }
}

/// WITNESS (BLOCKER 1, refusal direction B): the same save with the SECOND
/// fact's recorded snapshot restated as a NONCREATURE artifact. That snapshot
/// still satisfies the FIRST slot ("Destroy target artifact") and fails the
/// second ("deals 2 damage to any target", whose domain is a player or a
/// creature/planeswalker/battle). An identity-first-match lookup judges it
/// against the artifact requirement and admits it; pairing by slot refuses it.
#[test]
fn repeated_target_save_whose_second_fact_contradicts_its_own_slot_is_refused() {
    let parked = park_command_at_manual_payment();
    assert_repeated_target_reach_guards(&parked);

    let mut state = parked.runner.state().clone();
    {
        let pending = state
            .pending_cast
            .as_mut()
            .expect("reach-guard: the manual payment park keeps its pending cast");
        let facts = pending
            .committed_target_facts
            .as_deref_mut()
            .expect("reach-guard: the announcement latch is populated");
        match &mut facts[1] {
            ActivationTargetFact::Object { id, lki, .. } => {
                assert_eq!(
                    *id, parked.artifact_creature,
                    "reach-guard: the fact restated is the damage slot's own occurrence"
                );
                assert!(
                    lki.card_types.contains(&CoreType::Creature),
                    "reach-guard: the recorded snapshot says creature before the restatement"
                );
                lki.card_types.retain(|ty| *ty != CoreType::Creature);
                assert!(
                    lki.card_types.contains(&CoreType::Artifact),
                    "reach-guard: the restated snapshot STILL satisfies \"target artifact\", so \
                     only its OWN slot's requirement can refuse it"
                );
            }
            other => panic!("reach-guard: the second fact is an object fact, got {other:?}"),
        }
    }

    let error = PersistedGameState::capture(state)
        .into_game_state()
        .expect_err(
        "CR 601.2c: the damage slot's own requirement is checked, so a snapshot that fails it is \
         refused",
    );
    match error {
        PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason,
        } => {
            assert_eq!(spell_object_id, parked.spell, "the refusal names the cast");
            match reason {
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter { target } => {
                    assert_eq!(
                        target,
                        TargetRef::Object(parked.artifact_creature),
                        "and the target whose recorded announcement contradicts its own slot"
                    );
                }
                other => panic!(
                    "expected the declared-filter refusal that reads the recorded snapshot, got \
                     {other:?}"
                ),
            }
        }
        other => panic!("expected a recorded-announcement refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// BLOCKER 2 — a target on the stack is a real target (CR 405.1).
// ---------------------------------------------------------------------------

struct ParkedCounterspell {
    runner: GameRunner,
    counterspell: ObjectId,
    creature_spell: ObjectId,
    islands: Vec<ObjectId>,
}

/// CR 405.1 + CR 601.2c: P1's creature spell is on the stack; P0 casts
/// Counterspell at it with `CastPaymentMode::Manual`, which parks the cast at
/// `WaitingFor::ManaPayment` holding a latch whose one fact names the SPELL and
/// states the stack as the zone it was announced in.
fn park_counterspell_at_manual_payment() -> ParkedCounterspell {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature_to_hand(P1, "Bears", 2, 2).id();
    let counterspell = scenario
        .add_spell_to_hand_from_oracle(P0, "Counterspell", true, COUNTERSPELL_ORACLE)
        .with_mana_cost(blue_blue())
        .id();
    let islands = vec![
        scenario.add_basic_land(P0, ManaColor::Blue),
        scenario.add_basic_land(P0, ManaColor::Blue),
    ];
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let card_id = runner.state().objects[&creature].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: creature,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts its creature spell");
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Stack,
        "reach-guard: CR 405.1 puts the creature card on the stack"
    );
    // CR 117.3d: the active player passes so the defending player can respond.
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "reach-guard: P0 holds priority with P1's spell on the stack"
    );

    let card_id = runner.state().objects[&counterspell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: counterspell,
            card_id,
            targets: vec![creature],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the manual Counterspell cast is announced at the spell on the stack");
    assert!(
        matches!(cast.waiting_for, WaitingFor::ManaPayment { player: P0, .. }),
        "reach-guard: the cast parks at manual payment, got {:?}",
        cast.waiting_for
    );
    ParkedCounterspell {
        runner,
        counterspell,
        creature_spell: creature,
        islands,
    }
}

/// WITNESS (BLOCKER 2, honest direction): an untouched save of a Counterspell
/// announced at a spell, taken during mana payment, restores and resumes — and
/// the resumed cast still counters the stack target it announced.
#[test]
fn counterspell_save_taken_during_mana_payment_restores_and_resumes() {
    let parked = park_counterspell_at_manual_payment();

    // The reach guard: the latch states a STACK target, which is what the record
    // walk has to be able to answer.
    {
        let pending = parked_pending(&parked.runner);
        let facts = pending
            .committed_target_facts
            .as_deref()
            .expect("reach-guard: the announcement latch is populated at the payment park");
        assert_eq!(
            facts.len(),
            1,
            "CR 601.2c: Counterspell declares exactly one target"
        );
        match &facts[0] {
            ActivationTargetFact::Object { id, zone, .. } => {
                assert_eq!(*id, parked.creature_spell, "the latch names the spell");
                assert_eq!(
                    *zone,
                    AnnouncedTargetZone::Known(Zone::Stack),
                    "CR 405.1: the announced target was on the stack"
                );
            }
            other => panic!("reach-guard: the fact is an object fact, got {other:?}"),
        }
        assert_eq!(
            parked.runner.state().objects[&parked.creature_spell].zone,
            Zone::Stack,
            "reach-guard: the announced spell is still on the stack"
        );
        assert_eq!(
            pending.ability.effect.target_filter(),
            Some(&engine::types::ability::TargetFilter::StackSpell),
            "reach-guard: the declaring slot's filter is the stack predicate the record cannot \
             answer"
        );
    }

    let restored = save_and_restore(&parked.runner)
        .expect("CR 405.1: a stack target is a real target, so this save is admitted");
    let mut runner = GameRunner::from_state(restored);

    // CR 601.2c: the restore kept the stack target itself, not merely a cast
    // that will re-derive one.
    assert_eq!(
        parked_pending(&runner).ability.targets,
        vec![TargetRef::Object(parked.creature_spell)],
        "the restored cast still names the spell it announced"
    );

    for island in &parked.islands {
        let (_, _, grouped) = legal_actions_full(runner.state());
        let selection = grouped
            .get(island)
            .into_iter()
            .flatten()
            .find_map(|action| match action {
                GameAction::TapLandForMana { selection } => Some(selection.clone()),
                _ => None,
            })
            .expect("the restored payment prompt offers the Island's own {T}: Add {U}");
        runner
            .act(GameAction::TapLandForMana { selection })
            .expect("the restored cast takes the mana it announced it would");
    }
    runner
        .act(GameAction::PassPriority)
        .expect("the restored manual payment finalizes");
    assert_eq!(
        runner.state().objects[&parked.counterspell].zone,
        Zone::Stack,
        "reach-guard: the resumed cast reached the stack"
    );

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the counter resolved");
    assert_eq!(
        runner.state().objects[&parked.creature_spell].zone,
        Zone::Graveyard,
        "CR 701.6a: the resumed Counterspell counters the spell it announced"
    );
    assert_eq!(
        runner.state().objects[&parked.counterspell].zone,
        Zone::Graveyard,
        "and goes to its owner's graveyard"
    );
}

// ---------------------------------------------------------------------------
// THE REAL MODAL ROUTE — CR 601.2b's mode announcement, answered the way a
// player answers it, and the two declared slots that announcement produces.
// ---------------------------------------------------------------------------

/// CR 601.2b + CR 700.2: the modes the caster announces from Kolaghan's
/// Command's printed "Choose two —" block — printed mode 2, "Destroy target
/// artifact", and printed mode 3, "… deals 2 damage to any target". The pairing
/// walk judges exactly these two slots, in this printed order, one fact each.
const CHOSEN_MODE_INDICES: [usize; 2] = [2, 3];

struct ParkedModalCommand {
    runner: GameRunner,
    spell: ObjectId,
    artifact_creature: ObjectId,
    mountain: ObjectId,
    /// CR 601.2b: the mode-choice prompt the real cast raised. Kept so every
    /// witness can show the prompt was ANSWERED, not bypassed.
    modal: ModalChoice,
    /// CR 700.2 + CR 601.2b: the per-slot mode labels the targeting prompt
    /// posted — the engine's own statement of which chosen mode each declared
    /// slot belongs to (`mode_labels[i]` names `target_slots[i]`).
    slot_mode_labels: Vec<Option<String>>,
}

/// CR 601.2b + CR 601.2c: cast the PRINTED Kolaghan's Command, answer its real
/// mode-choice prompt with "Destroy target artifact" + "… deals 2 damage to any
/// target", then select the SAME legal artifact creature in the two declared
/// slots those modes produce. `CastPaymentMode::Manual` parks the completed
/// announcement at `WaitingFor::ManaPayment` holding its announced-target latch
/// — the state a save at the payment boundary captures.
fn park_modal_command_at_manual_payment() -> ParkedModalCommand {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact_creature = scenario
        .add_creature(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let spell = scenario
        // The card as printed: the "Choose two —" block is the ability the
        // caster actually announces, so the mode choice is a real prompt.
        .add_spell_to_hand_from_oracle(P0, "Kolaghan's Command", true, KOLAGHANS_COMMAND_ORACLE)
        .with_mana_cost(red())
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the manual Kolaghan's Command cast is announced");

    let mut modal: Option<ModalChoice> = None;
    let mut slot_mode_labels: Vec<Option<String>> = Vec::new();
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            // CR 601.2b: "If the spell is modal, the player announces the mode
            // choice" — the prompt this cast raises before any target is chosen.
            WaitingFor::ModeChoice { modal: offered, .. } => {
                assert!(
                    modal.is_none(),
                    "reach-guard: a casting's mode choice is announced exactly once"
                );
                modal = Some(offered);
                runner
                    .act(GameAction::SelectModes {
                        indices: CHOSEN_MODE_INDICES.to_vec(),
                    })
                    .expect("CR 700.2: two modes of a 'Choose two —' spell are accepted");
            }
            WaitingFor::TargetSelection {
                mode_labels: labels,
                ..
            } => {
                slot_mode_labels = labels;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(artifact_creature)),
                    })
                    .expect("CR 601.2c: the same artifact creature fits both declared slots");
            }
            WaitingFor::ManaPayment { player, .. } => {
                assert_eq!(player, P0, "the caster pays");
                break;
            }
            other => panic!("unexpected prompt while announcing the two modes: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "reach-guard: the cast parks at manual payment, got {:?}",
        runner.state().waiting_for
    );
    ParkedModalCommand {
        runner,
        spell,
        artifact_creature,
        mountain,
        modal: modal.expect("reach-guard: CR 601.2b's mode choice was raised and answered"),
        slot_mode_labels,
    }
}

/// Reach guards shared by the modal witnesses: the mode choice offered the
/// printed four-mode block, the two slots the chosen modes declare carry
/// DIFFERENT requirements, the engine labels each slot with the mode that
/// declared it, and the latch holds one fact per declared slot, both naming the
/// SAME object.
fn assert_modal_repeated_target_reach_guards(parked: &ParkedModalCommand) {
    // CR 700.2: "Choose two —". A four-mode block, two of them to be announced.
    assert_eq!(
        (parked.modal.min_choices, parked.modal.max_choices),
        (2, 2),
        "reach-guard: the printed header is \"Choose two —\", got {:?}",
        parked.modal.mode_descriptions,
    );
    assert_eq!(
        parked.modal.mode_count, 4,
        "reach-guard: Kolaghan's Command prints four modes, got {:?}",
        parked.modal.mode_descriptions
    );
    // The parsed block IS the printed block, in printed order: return, discard,
    // destroy, damage. Asserting all four — not only the two announced — is what
    // makes the indices in this file printed indices rather than fixture-local
    // ones: a fixture reordered to put "Destroy target artifact" first would fail
    // here instead of silently re-labelling the slots.
    for (index, expected) in [
        (
            0,
            "Return target creature card from your graveyard to your hand",
        ),
        (1, "Target player discards a card"),
        (2, "Destroy target artifact"),
        (3, "deals 2 damage to any target"),
    ] {
        let description = parked
            .modal
            .mode_descriptions
            .get(index)
            .unwrap_or_else(|| panic!("reach-guard: printed mode {index} parses"));
        assert!(
            description.contains(expected),
            "reach-guard: printed mode {index} is {expected:?}, got {description:?}"
        );
    }

    let pending = parked_pending(&parked.runner);
    let root = pending.ability.as_ref();
    let second = root
        .sub_ability
        .as_deref()
        .expect("reach-guard: the two chosen modes are a two-instruction chain");
    let first_filter = root
        .effect
        .target_filter()
        .expect("reach-guard: the artifact-destruction mode declares a target");
    let second_filter = second
        .effect
        .target_filter()
        .expect("reach-guard: the damage mode declares a target");
    assert_ne!(
        first_filter, second_filter,
        "reach-guard: the mode-built chain keeps each chosen mode's OWN requirement — which is \
         what makes the identity-first-match lookup judge the damage fact against the artifact \
         requirement"
    );

    // CR 700.2 + CR 601.2b: the targeting prompt names the mode each slot came
    // from. This is the engine's own mode→slot mapping, read back before the
    // save: the destruction slot is first (printed mode 2), the damage slot
    // second (printed mode 3) — the order the pairing walk assumes.
    assert_eq!(
        parked.slot_mode_labels.len(),
        2,
        "reach-guard: the two chosen modes declare two slots, got {:?}",
        parked.slot_mode_labels
    );
    let label = |index: usize| {
        parked.slot_mode_labels[index]
            .as_deref()
            .unwrap_or_else(|| panic!("reach-guard: slot {index} carries its mode label"))
    };
    assert!(
        label(0).contains("Destroy target artifact"),
        "reach-guard: the first declared slot belongs to printed mode 2, got {:?}",
        label(0)
    );
    assert!(
        label(1).contains("2 damage to any target"),
        "reach-guard: the second declared slot belongs to printed mode 3, got {:?}",
        label(1)
    );

    let facts = pending
        .committed_target_facts
        .as_deref()
        .expect("reach-guard: the announcement latch is populated at the payment park");
    assert_eq!(
        facts.len(),
        2,
        "CR 601.2c: two declared slots owe two facts, got {facts:?}"
    );
    for (index, fact) in facts.iter().enumerate() {
        match fact {
            ActivationTargetFact::Object { id, zone, .. } => {
                assert_eq!(
                    *id, parked.artifact_creature,
                    "reach-guard: fact {index} names the one object both slots chose"
                );
                assert_eq!(
                    *zone,
                    AnnouncedTargetZone::Known(Zone::Battlefield),
                    "reach-guard: the artifact creature was announced on the battlefield"
                );
            }
            other => panic!("reach-guard: fact {index} is an object fact, got {other:?}"),
        }
    }
}

/// WITNESS (real modal route, honest direction): cast the printed Kolaghan's
/// Command, announce "Destroy target artifact" + "… deals 2 damage to any
/// target", choose the same artifact creature in both declared slots, then save
/// during mana payment. The save restores and resumes, and the resumed cast
/// still acts on the announcement the player really made.
#[test]
fn modal_command_save_restores_and_resumes() {
    let parked = park_modal_command_at_manual_payment();
    assert_modal_repeated_target_reach_guards(&parked);

    let restored = save_and_restore(&parked.runner)
        .expect("CR 601.2c: an honest repeated-target latch is admitted");
    let mut runner = GameRunner::from_state(restored);

    // The restore kept the declaration the mode choice built, not merely a cast
    // that will re-derive one: both chosen modes are still the chain, and both
    // named occurrences are still the same object.
    {
        let pending = parked_pending(&runner);
        assert_eq!(
            pending.ability.targets,
            vec![TargetRef::Object(parked.artifact_creature)],
            "CR 601.2c: the restored chain still names the artifact creature in its first slot"
        );
        assert_eq!(
            pending
                .ability
                .sub_ability
                .as_deref()
                .expect("reach-guard: the damage mode survives the restore")
                .targets,
            vec![TargetRef::Object(parked.artifact_creature)],
            "CR 601.2c: and in the damage mode's own slot"
        );
        assert_eq!(
            pending
                .committed_target_facts
                .as_deref()
                .map(|facts| facts.len()),
            Some(2),
            "CR 601.2c: the restored latch still holds one fact per declared slot"
        );
    }

    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&parked.mountain)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the restored payment prompt offers the Mountain's own {T}: Add {R}");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("the restored cast takes the mana it announced it would");
    runner
        .act(GameAction::PassPriority)
        .expect("the restored manual payment finalizes");
    assert_eq!(
        runner.state().objects[&parked.spell].zone,
        Zone::Stack,
        "reach-guard: the resumed cast reached the stack"
    );

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the spell resolved");
    assert_eq!(
        runner.state().objects[&parked.artifact_creature].zone,
        Zone::Graveyard,
        "CR 701.8a: the resumed cast destroys the artifact its first mode announced"
    );
    let recorded_targets: Option<Vec<TargetRef>> = runner
        .state()
        .spells_cast_this_turn_by_player
        .values()
        .flat_map(|records| records.iter())
        .find(|record| record.spell_object_id == Some(parked.spell))
        .and_then(|record| record.targets.as_deref())
        .map(|facts| facts.iter().map(fact_target_ref).collect());
    assert_eq!(
        recorded_targets,
        Some(vec![
            TargetRef::Object(parked.artifact_creature),
            TargetRef::Object(parked.artifact_creature),
        ]),
        "CR 601.2i: the resumed cast publishes BOTH announced occurrences"
    );
}

/// WITNESS (real modal route, refusal direction): the same save with the SECOND
/// declared slot's fact restated as a NONCREATURE artifact. That snapshot still
/// satisfies the destruction mode ("Destroy target artifact") and fails the
/// damage mode ("deals 2 damage to any target", whose domain is a player or a
/// creature/planeswalker/battle) — so an identity-first-match lookup, which
/// judges every fact naming that object against the FIRST slot, admits it. The
/// pairing walk reads the slot the fact came from, which the mode labels above
/// pin to printed mode 3, and refuses it.
#[test]
fn modal_command_save_whose_damage_slot_fact_contradicts_its_mode_is_refused() {
    let parked = park_modal_command_at_manual_payment();
    assert_modal_repeated_target_reach_guards(&parked);

    let mut state = parked.runner.state().clone();
    {
        let pending = state
            .pending_cast
            .as_mut()
            .expect("reach-guard: the manual payment park keeps its pending cast");
        let facts = pending
            .committed_target_facts
            .as_deref_mut()
            .expect("reach-guard: the announcement latch is populated");
        match &mut facts[1] {
            ActivationTargetFact::Object { id, lki, .. } => {
                assert_eq!(
                    *id, parked.artifact_creature,
                    "reach-guard: the fact restated is the damage mode's own occurrence"
                );
                assert!(
                    lki.card_types.contains(&CoreType::Creature),
                    "reach-guard: the recorded snapshot says creature before the restatement"
                );
                lki.card_types.retain(|ty| *ty != CoreType::Creature);
                assert!(
                    lki.card_types.contains(&CoreType::Artifact),
                    "reach-guard: the restated snapshot STILL satisfies \"target artifact\", so \
                     only the damage mode's own requirement can refuse it"
                );
            }
            other => panic!("reach-guard: the second fact is an object fact, got {other:?}"),
        }
    }

    let error = PersistedGameState::capture(state)
        .into_game_state()
        .expect_err(
            "CR 601.2c: the damage mode's own requirement is checked, so a snapshot that fails \
             it is refused",
        );
    match error {
        PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason,
        } => {
            assert_eq!(spell_object_id, parked.spell, "the refusal names the cast");
            match reason {
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter { target } => {
                    assert_eq!(
                        target,
                        TargetRef::Object(parked.artifact_creature),
                        "and the target whose recorded announcement contradicts its own slot"
                    );
                }
                other => panic!(
                    "expected the declared-filter refusal that reads the recorded snapshot, got \
                     {other:?}"
                ),
            }
        }
        other => panic!("expected a recorded-announcement refusal, got {other:?}"),
    }
}
