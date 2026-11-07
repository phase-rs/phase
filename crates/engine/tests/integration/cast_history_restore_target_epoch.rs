//! CR 115.1 + CR 601.2c + CR 601.2i + CR 400.7: a save parked at the ordinary
//! manual payment prompt must either carry the announced-target facts or be
//! refused at the restore boundary — never be resumed into a post-payment raw-id
//! re-read of a target that a cost already moved.
//!
//! Printed **Devour in Flames** is `{2}{R}` for "As an additional cost to cast
//! this spell, return a land you control to its owner's hand. / Devour in Flames
//! deals 5 damage to target creature or planeswalker." (Scryfall, oracle text
//! verified) — so it can target and return the very same land. Printed **Mishra's
//! Factory** is a Land whose own `{1}` ability makes it "a 2/2 Assembly-Worker
//! artifact creature until end of turn. It's still a land."; returned to hand it
//! reverts to its printed Land type line, so a capture taken after the return
//! describes a different object than the one the caster announced (CR 400.7).
//!
//! Three packs, all reached through the real `SelectTargets` / `SelectCards` /
//! `ManaPayment` prompts with the caster announcing the target manually:
//!
//! * **intact** — the pending cast carries `committed_target_facts`. It is
//!   admitted, and the RESUMED SAVE (not the pre-save runner) writes a CR 601.2i
//!   record that names the animated Factory as the creature it was.
//! * **missing latch** — the same pack with `committed_target_facts` stripped,
//!   which is the shape a pre-latch save has. It must be refused with
//!   `PersistedRestoreError::UnknownPendingTargetFacts`.
//! * **pre-declaration** — a pack taken at the `TargetSelection` prompt, before
//!   CR 601.2c, legitimately has no facts and must still be admitted (the
//!   over-rejection guard), and resuming it still records the announced target.
//!
//! The distinction between "not declared yet" and "declared, unlatched, and
//! unrecoverable" is carried by the cast's own declared-target record and its
//! announcement-time incarnation pin — the same authorities the latch captures
//! from — not by a new flag: the refusal fires exactly when a declared object
//! target's pin no longer names the live object (CR 400.7).

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    ActivationTargetFact, CastPaymentMode, GameState, PersistedGameState, PersistedRestoreError,
    SpellCastRecord, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DEVOUR_IN_FLAMES: &str =
    "As an additional cost to cast this spell, return a land you control to its owner's hand.\nDevour in Flames deals 5 damage to target creature or planeswalker.";

/// Printed Mishra's Factory, verbatim: the `{1}` activation is the animation
/// that makes it an artifact creature while it remains a land.
const MISHRA_S_FACTORY: &str = "{T}: Add {C}.\n{1}: This land becomes a 2/2 Assembly-Worker artifact creature until end of turn. It's still a land.\n{T}: Target Assembly-Worker creature gets +1/+1 until end of turn.";

fn pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|t| ManaUnit::new(*t, ObjectId(0), false, vec![]))
        .collect()
}

/// The board every witness below parks on: P0's animated Factory is BOTH a legal
/// "target creature or planeswalker" and a legal "land you control" to return,
/// and P1's Bears is the second legal target that keeps the engine from
/// auto-selecting the only one.
fn devour_board() -> (GameScenario, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let land = sc
        .add_land_from_oracle(P0, "Mishra's Factory", MISHRA_S_FACTORY)
        .id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Devour in Flames", false, DEVOUR_IN_FLAMES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    sc.add_creature(P1, "Grizzly Bears", 2, 2);
    // CR 601.2h: mana is supplied up front so the additional cost is the only
    // thing the cast has to ask for. The `{1}` animation below spends a fourth.
    sc.with_mana_pool(
        P0,
        pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Red,
        ]),
    );
    (sc, spell, land)
}

/// CR 613.1d: activate the printed `{1}` animation so the land is a 2/2
/// artifact creature that is still a land — the announced target. Layer 4 is the
/// type-changing layer, which is the only rule this needs: the animation adds a
/// card type and a subtype, and touching a land's *land* subtype is CR 305.7's
/// business, not this effect's.
fn animate(runner: &mut GameRunner, land: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reach: Mishra's Factory activates its animation");
    settle_stack(runner);
    assert!(
        runner.state().objects[&land]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach guard: the animation made the Factory a creature"
    );
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the cast is announced");
}

fn prompt_name(waiting_for: &WaitingFor) -> String {
    format!("{waiting_for:?}")
        .split([' ', '{', '('])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Answers each announcement prompt through the real prompts — `SelectTargets`
/// for CR 601.2c, `SelectCards` for the return-to-hand additional cost, and
/// `PassPriority` for the interactive mana payment — stopping when `stop_at`
/// names the prompt to park on (which it returns as the last answered name).
fn drive(
    runner: &mut GameRunner,
    target: ObjectId,
    land: ObjectId,
    stop_at: Option<&str>,
) -> Vec<String> {
    let mut prompts = Vec::new();
    for _ in 0..32 {
        let wf = runner.state().waiting_for.clone();
        let name = prompt_name(&wf);
        if stop_at == Some(name.as_str()) {
            prompts.push(name);
            return prompts;
        }
        let action = match wf {
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => return prompts,
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            },
            WaitingFor::PayCost { .. } => GameAction::SelectCards { cards: vec![land] },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            other => panic!("unexpected announcement prompt {other:?}"),
        };
        prompts.push(name);
        runner.act(action).expect("the prompt accepts its answer");
    }
    panic!("the announcement did not settle");
}

/// Pass priority until the stack is empty again.
fn settle_stack(runner: &mut GameRunner) {
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes while the stack drains");
    }
    panic!("the stack did not settle");
}

/// CR 601.2c + CR 601.2h: cast Devour in Flames manually and return the animated
/// Factory it targets. The reach guards pin that the target was announced through
/// the public prompts, that the cost really moved it to hand, and — the point of
/// the whole file — that the card now occupying that storage id is no longer a
/// creature.
fn devour_returning_its_target(stop_at: Option<&str>) -> (GameRunner, ObjectId, ObjectId) {
    let (sc, spell, land) = devour_board();
    let mut runner = sc.build();
    animate(&mut runner, land);
    cast(&mut runner, spell);
    let prompts = drive(&mut runner, land, land, stop_at);
    assert!(
        prompts.iter().any(|prompt| prompt == "TargetSelection"),
        "reach guard: the target was announced through the public `SelectTargets` prompt; \
         prompts {prompts:?}"
    );
    if !matches!(stop_at, Some("TargetSelection" | "PayCost")) {
        assert!(
            prompts.iter().any(|prompt| prompt == "PayCost"),
            "reach guard: the return-to-hand additional cost was paid through the public \
             `SelectCards` prompt; prompts {prompts:?}"
        );
        assert_eq!(
            runner.state().objects[&land].zone,
            Zone::Hand,
            "reach guard: the additional cost returned the targeted land to its owner's hand; \
             prompts {prompts:?}"
        );
        assert!(
            !runner.state().objects[&land]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "anti-vacuity: the returned card's own type line is no longer a creature, so a \
             post-payment raw-id capture would record Land"
        );
    }
    (runner, spell, land)
}

/// CR 601.2c + CR 601.2h: the SAME cast parked one prompt earlier — at the
/// return-a-land `PayCost` prompt, with the target declared and no cost paid yet.
/// The announced creature is still the live object, which is the whole point:
/// this park is the one where the announcement can still be read.
fn devour_parked_at_return_cost() -> (GameRunner, ObjectId, ObjectId) {
    let (runner, spell, land) = devour_returning_its_target(Some("PayCost"));
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "reach guard: the return-to-hand cost prompt owns the cast, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "reach guard: no cost has been paid at this prompt, so the announced target is still \
         on the battlefield"
    );
    assert!(
        runner.state().objects[&land]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach guard: the announced target is still the animated creature"
    );
    (runner, spell, land)
}

/// The one latched object fact of `spell`'s committed cast record.
fn declared_object_fact(state: &GameState, spell: ObjectId) -> ActivationTargetFact {
    recorded_cast(state, spell)
        .declared_targets()
        .iter()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { .. }))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "reach: the record declares the announced object target; got {:?}",
                recorded_cast(state, spell).declared_targets()
            )
        })
}

fn fact_core_types(fact: &ActivationTargetFact) -> Vec<CoreType> {
    match fact {
        ActivationTargetFact::Object { lki, .. } => lki.card_types.clone(),
        ActivationTargetFact::Player(_) => Vec::new(),
    }
}

fn fact_object_id(fact: &ActivationTargetFact) -> ObjectId {
    match fact {
        ActivationTargetFact::Object { id, .. } => *id,
        ActivationTargetFact::Player(player) => {
            panic!("the fact is an object, got player {player:?}")
        }
    }
}

/// The recorded cast of `spell`, found by the stable id link.
fn recorded_cast(state: &GameState, spell: ObjectId) -> &SpellCastRecord {
    state
        .spells_cast_this_turn_by_player
        .values()
        .flat_map(|records| records.iter())
        .find(|record| record.spell_object_id == Some(spell))
        .expect("reach: the committed cast is recorded in the turn history")
}

/// The serialized pack a save of `state` produces.
fn persisted_json(state: &GameState) -> serde_json::Value {
    serde_json::to_value(PersistedGameState::capture(state.clone())).expect("the park serializes")
}

fn admitted(json: serde_json::Value) -> Result<GameState, PersistedRestoreError> {
    serde_json::from_value::<PersistedGameState>(json)
        .expect("the park decodes")
        .into_game_state()
}

/// Remove the persisted PENDING CAST's announced-target latch, which reproduces a
/// save written before it existed. The field is an `Option`, so the stripped pack
/// still decodes — it is the admission decision, not serde, that must catch it.
/// Only this carrier is stripped: the same key also rides the `spell_cast_reversal`
/// undo journal's `before` snapshots, which are replayed edits rather than
/// admission input.
fn strip_pending_latch(json: &mut serde_json::Value) -> bool {
    json.get_mut("state")
        .and_then(|state| state.get_mut("pending_cast"))
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|pending| pending.remove("committed_target_facts"))
        .is_some()
}

/// Remove the announced-target latch from every ADMISSION carrier a parked cast
/// writes — `state.pending_cast` (the `External` owner), the pending embedded in
/// `state.waiting_for` (the `Prompt` owner), and the resume roots that hold one
/// (`pending_cost_move_resume`, `pending_deferred_life_cost_resume`,
/// `pending_discard_for_cost`), including the `prepaid_finalization` pre-payment
/// checks and cost-move completion payloads nested inside them. That is the state
/// a save written before the latch existed has, and it is the state a `PayCost`
/// park needs: that prompt keeps a second copy of the cast on
/// `WaitingFor::PayCost`'s `CostResume::Spell`, so stripping only
/// `state.pending_cast` would leave the facts in place and test nothing.
///
/// The carrier list is deliberately the admission walk's, not the whole document:
/// the same key also rides the `spell_cast_reversal` undo journal's `before`
/// snapshots, which are replayed edits rather than admission input, and a parked
/// cast carries one of those from announcement until commit.
fn strip_admission_latches(json: &mut serde_json::Value) -> usize {
    fn strip(value: &mut serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(map) => {
                let mut stripped = usize::from(map.remove("committed_target_facts").is_some());
                for nested in map.values_mut() {
                    stripped += strip(nested);
                }
                stripped
            }
            serde_json::Value::Array(items) => items.iter_mut().map(strip).sum::<usize>(),
            _ => 0,
        }
    }
    let Some(state) = json
        .get_mut("state")
        .and_then(|state| state.as_object_mut())
    else {
        return 0;
    };
    let mut stripped = 0;
    for carrier in [
        "pending_cast",
        "waiting_for",
        "pending_cost_move_resume",
        "pending_deferred_life_cost_resume",
        "pending_discard_for_cost",
    ] {
        if let Some(value) = state.get_mut(carrier) {
            stripped += strip(value);
        }
    }
    stripped
}

/// CR 601.2c + CR 115.1 + CR 601.2h: a save parked on the ordinary manual
/// `ManaPayment` prompt of a cast whose target a cost already moved must keep the
/// announcement-time facts, and the RESUMED SAVE must write them.
#[test]
fn a_manual_payment_save_keeps_its_announced_target_through_resume() {
    let (runner, spell, land) = devour_returning_its_target(Some("ManaPayment"));

    // Reach guard: the parked cast is the interactive mana payment, and it already
    // carries the announced-target latch taken at the declaration boundary.
    let parked = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach guard: the ManaPayment prompt owns the cast");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the cast is parked at the ordinary manual payment prompt, got {:?}",
        runner.state().waiting_for
    );
    let latched = parked
        .committed_target_facts
        .as_deref()
        .expect("reach guard: the parked cast carries its announced-target latch");
    let latched_fact = latched
        .iter()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == land))
        .unwrap_or_else(|| {
            panic!("reach guard: the latch names the announced Factory; got {latched:?}")
        });
    assert!(
        fact_core_types(latched_fact).contains(&CoreType::Creature),
        "reach guard: the latch recorded the animated creature, not the card in hand: {:?}",
        fact_core_types(latched_fact)
    );

    let json = persisted_json(runner.state());
    let restored = admitted(json).map_err(|error| {
        format!("a manual-payment pack carrying its announced targets must be admitted: {error:?}")
    });
    let restored_state =
        restored.expect("reach: a pack with the latch is admitted by the restore boundary");

    // The positive continuation resumes the RESTORED state, not the pre-save runner.
    let mut resumed = GameRunner::from_state(restored_state);
    assert!(
        matches!(resumed.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the restored state still owes the interactive mana payment, got {:?}",
        resumed.state().waiting_for
    );
    resumed
        .act(GameAction::PassPriority)
        .expect("the restored save pays its locked cost");
    assert!(
        resumed.state().stack.iter().any(|entry| entry.id == spell),
        "reach guard: the resumed save put the spell on the stack"
    );

    let fact = declared_object_fact(resumed.state(), spell);
    assert_eq!(
        fact_object_id(&fact),
        land,
        "the resumed save's record names the announced target"
    );
    assert!(
        fact_core_types(&fact).contains(&CoreType::Creature),
        "the resumed save's record keeps the selection-time creature the caster announced, \
         not the returned card's printed type line: {:?}",
        fact_core_types(&fact)
    );
}

/// CR 601.2c + CR 115.1 + CR 400.7: the same pack with the latch stripped is the
/// shape a pre-latch save has. The cast has declared its target, so the facts are
/// unavailable and unrecoverable — a raw-id re-read now describes the card in hand
/// — and the save must be refused at the shared restore boundary rather than
/// resumed into a falsely known cast history.
#[test]
fn a_manual_payment_save_without_the_latch_is_refused() {
    let (runner, spell, _land) = devour_returning_its_target(Some("ManaPayment"));

    // Reach guard: the declared target's announcement-time pin no longer names the
    // live object (CR 400.7), which is what makes a raw-id re-read describe the
    // card in hand instead of the announced creature.
    let parked = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach guard: the ManaPayment prompt owns the cast");
    let pin = parked
        .ability
        .target_pins
        .first()
        .copied()
        .flatten()
        .expect("reach guard: the announced target carries an incarnation pin");
    assert!(
        !pin.is_current(runner.state()),
        "reach guard: the announced target already moved, so its facts cannot be re-read"
    );

    let mut legacy = persisted_json(runner.state());
    assert!(
        strip_pending_latch(&mut legacy),
        "reach: the fixture really carried a pending-cast latch to strip"
    );

    let refused = admitted(legacy);
    assert!(
        matches!(
            refused,
            Err(PersistedRestoreError::UnknownPendingTargetFacts {
                spell_object_id: Some(id),
            }) if id == spell
        ),
        "a post-declaration pack without announced-target facts is refused, naming the \
         spell: {:?}",
        refused.err()
    );
}

/// CR 601.2a–b + CR 601.2c: a pack taken while the engine is still asking for the
/// target has declared nothing, so `committed_target_facts: None` is the honest
/// state and the save must be ADMITTED. This is the over-rejection guard: the
/// refusal above must not also swallow the pre-declaration prompt.
#[test]
fn a_pre_declaration_pack_is_admitted_and_still_records_its_target() {
    let (runner, spell, land) = devour_returning_its_target(Some("TargetSelection"));

    let prompt_cast = match &runner.state().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast,
        other => panic!("reach guard: the target-declaration prompt, got {other:?}"),
    };
    assert!(
        prompt_cast.committed_target_facts.is_none(),
        "reach guard: nothing has latched yet at the target-declaration prompt"
    );
    assert!(
        engine::game::ability_utils::declared_targets_in_chain(&prompt_cast.ability).is_empty(),
        "reach guard: the cast has not declared a target yet, so there is no fact to lose"
    );

    let restored_state = admitted(persisted_json(runner.state())).map_err(|error| {
        format!("a pre-declaration pack declares nothing and must be admitted: {error:?}")
    });
    let restored_state =
        restored_state.expect("reach: the pre-declaration prompt is admitted, not refused");

    // Resuming the restored pre-declaration prompt still announces the target and
    // writes it: the latch is taken at the declaration boundary after the restore.
    let mut resumed = GameRunner::from_state(restored_state);
    let prompts = drive(&mut resumed, land, land, None);
    assert!(
        prompts.iter().any(|prompt| prompt == "TargetSelection"),
        "reach guard: the restored prompt still asks for the target; prompts {prompts:?}"
    );
    assert!(
        matches!(resumed.state().waiting_for, WaitingFor::Priority { .. }),
        "reach guard: the resumed announcement settled, got {:?}",
        resumed.state().waiting_for
    );
    assert_eq!(
        resumed.state().objects[&land].zone,
        Zone::Hand,
        "reach guard: the returned land paid the additional cost"
    );

    let fact = declared_object_fact(resumed.state(), spell);
    assert_eq!(
        fact_object_id(&fact),
        land,
        "the resumed pre-declaration save names the announced target"
    );
    assert!(
        fact_core_types(&fact).contains(&CoreType::Creature),
        "the resumed pre-declaration save keeps the selection-time creature: {:?}",
        fact_core_types(&fact)
    );
}

/// CR 601.2c + CR 115.1 + CR 400.7: the gap the narrowing above leaves open, and
/// why admission RECOVERS instead of only refusing.
///
/// Same Devour in Flames fixture, one prompt earlier: parked at the
/// return-a-land `PayCost` prompt, the target is declared and nothing has been
/// paid, so the animated Factory is still on the battlefield and the cast's
/// declaration pins still name it. A save written before the latch existed (every
/// latch stripped) therefore still proves exactly what was announced, and the
/// honest outcome is to recover it. Merely admitting the pack resumes into the
/// raw-id fallback and records the printed **Land**; merely refusing it rejects a
/// park the engine itself produces.
#[test]
fn a_return_cost_park_recovers_its_announced_target_and_records_the_creature() {
    let (runner, spell, land) = devour_parked_at_return_cost();

    // Reach guard: the announced target's pin is CURRENT, so the announcement is
    // readable from the save — this is the state the refusal-only rule let through.
    let parked = runner
        .state()
        .pending_cast
        .as_deref()
        .or_else(|| runner.state().waiting_for.pending_cast_ref())
        .expect("reach guard: the return-cost prompt owns the cast");
    let pin = parked
        .ability
        .target_pins
        .first()
        .copied()
        .flatten()
        .expect("reach guard: the announced target carries an incarnation pin");
    assert!(
        pin.is_current(runner.state()),
        "reach guard: the announced target has not moved yet, so its facts are recoverable"
    );

    let json = persisted_json(runner.state());
    // Reach guard: the fixture really does carry the undo journal, which is why
    // the strip below is scoped to the admission carriers rather than the whole
    // document.
    assert!(
        serde_json::to_string(&json)
            .expect("the pack serializes")
            .contains("spell_cast_reversal"),
        "reach guard: a parked cast carries its undo journal, which the strip must not touch"
    );
    let mut legacy = json.clone();
    assert!(
        strip_admission_latches(&mut legacy) > 0,
        "reach: the fixture really carried a latch to strip"
    );

    let restored_state = admitted(legacy).map_err(|error| {
        format!(
            "a return-cost park that still proves its announcement is recovered, not refused: \
             {error:?}"
        )
    });
    let restored_state =
        restored_state.expect("reach: the recoverable park is admitted with recovered facts");

    // The recovered latch is on the admitted park itself, which is what the
    // resumed continuation carries to the CR 601.2i write.
    let recovered = restored_state
        .pending_cast
        .as_deref()
        .or_else(|| restored_state.waiting_for.pending_cast_ref())
        .expect("reach guard: the restored park still owns its cast")
        .committed_target_facts
        .as_deref()
        .expect("the admission recovered the announced-target latch");
    let recovered_fact = recovered
        .iter()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == land))
        .unwrap_or_else(|| {
            panic!("the recovered latch names the announced Factory; got {recovered:?}")
        });
    assert!(
        fact_core_types(recovered_fact).contains(&CoreType::Creature),
        "the recovered latch describes the announcement-time creature: {:?}",
        fact_core_types(recovered_fact)
    );

    let mut resumed = GameRunner::from_state(restored_state);
    let prompts = drive(&mut resumed, land, land, None);
    assert!(
        prompts.iter().any(|prompt| prompt == "PayCost"),
        "reach guard: the restored save still owes the return-to-hand cost; prompts {prompts:?}"
    );
    assert_eq!(
        resumed.state().objects[&land].zone,
        Zone::Hand,
        "reach guard: the resumed save paid the return-to-hand cost; prompts {prompts:?}"
    );
    assert!(
        resumed.state().stack.iter().any(|entry| entry.id == spell),
        "reach guard: the resumed save put the spell on the stack; prompts {prompts:?}"
    );

    let fact = declared_object_fact(resumed.state(), spell);
    assert_eq!(
        fact_object_id(&fact),
        land,
        "the resumed save's record names the announced target"
    );
    assert!(
        fact_core_types(&fact).contains(&CoreType::Creature),
        "the resumed save records the modification-time creature the caster announced, not the \
         returned card's printed type line: {:?}",
        fact_core_types(&fact)
    );
}

/// CR 601.2d: Printed **Violent Eruption** is "{1}{R}{R}{R} — Violent Eruption
/// deals 4 damage divided as you choose among any number of targets." (Scryfall,
/// oracle text verified).
const VIOLENT_ERUPTION: &str =
    "Violent Eruption deals 4 damage divided as you choose among any number of targets.";

/// The board for the division witness: the animated Factory is a legal "any
/// target", P1's Bears keeps the engine from auto-selecting the only one, and the
/// pool covers the `{1}` animation plus `{1}{R}{R}{R}`.
fn eruption_board() -> (GameScenario, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let land = sc
        .add_land_from_oracle(P0, "Mishra's Factory", MISHRA_S_FACTORY)
        .id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Violent Eruption", true, VIOLENT_ERUPTION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red, ManaCostShard::Red],
            generic: 1,
        })
        .id();
    sc.add_creature(P1, "Grizzly Bears", 2, 2);
    sc.with_mana_pool(
        P0,
        pool(&[
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Colorless,
        ]),
    );
    (sc, spell, land)
}

/// CR 601.2c + CR 601.2d: drive the cast to its `DistributeAmong` prompt through
/// the slot-by-slot `ChooseTarget` prompt the client uses — announce `target`,
/// then decline the optional tail — and return the prompts answered so far, the
/// last one being the division.
fn drive_to_distribution(runner: &mut GameRunner, target: ObjectId) -> Vec<String> {
    let mut prompts = Vec::new();
    for _ in 0..32 {
        let wf = runner.state().waiting_for.clone();
        let name = prompt_name(&wf);
        match wf {
            WaitingFor::DistributeAmong { .. } => {
                prompts.push(name);
                return prompts;
            }
            WaitingFor::TargetSelection { .. } => {
                let action = if prompts.iter().any(|prompt| prompt == "TargetSelection") {
                    GameAction::ChooseTarget { target: None }
                } else {
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    }
                };
                prompts.push(name);
                runner.act(action).expect("the prompt accepts its target");
            }
            other => panic!("unexpected prompt before the division: {other:?}"),
        }
    }
    panic!("the cast never reached its division");
}

/// CR 601.2d + CR 601.2c + CR 400.7: a cast parked at `DistributeAmong` has
/// declared its targets and paid nothing, and it resumes the payment pipeline
/// from the pending installed on `state.pending_cast` rather than from the
/// interactive declaration boundary. That park therefore owes the
/// announced-target latch itself — "declared ⇒ latched" is the invariant restore
/// admission reads — and both the latched pack and a pre-latch pack (every latch
/// stripped, pins still current) must be admitted and record the announcement-time
/// creature.
#[test]
fn a_distribution_park_is_latched_and_records_its_announced_target() {
    let (sc, spell, land) = eruption_board();
    let mut runner = sc.build();
    animate(&mut runner, land);
    cast(&mut runner, spell);
    let prompts = drive_to_distribution(&mut runner, land);
    assert_eq!(
        prompts.last().map(String::as_str),
        Some("DistributeAmong"),
        "reach guard: the cast paused for its division; prompts {prompts:?}"
    );

    let parked = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach guard: the division prompt owns the cast on `state.pending_cast`");
    let latched = parked.committed_target_facts.as_deref().unwrap_or_else(|| {
        panic!("the division park carries the announced-target latch; got {parked:?}")
    });
    let latched_fact = latched
        .iter()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == land))
        .unwrap_or_else(|| panic!("the latch names the announced Factory; got {latched:?}"));
    assert!(
        fact_core_types(latched_fact).contains(&CoreType::Creature),
        "the division park latched the animated creature: {:?}",
        fact_core_types(latched_fact)
    );

    // (a) The latched pack is admitted and the resumed save records the creature.
    let restored_state = admitted(persisted_json(runner.state())).map_err(|error| {
        format!("a division park carrying its announced targets must be admitted: {error:?}")
    });
    let restored_state = restored_state.expect("reach: the latched division park is admitted");
    let mut resumed = GameRunner::from_state(restored_state);
    assert!(
        matches!(
            resumed.state().waiting_for,
            WaitingFor::DistributeAmong { .. }
        ),
        "reach guard: the restored save still owns its division, got {:?}",
        resumed.state().waiting_for
    );
    resumed
        .act(GameAction::DistributeAmong {
            distribution: vec![(TargetRef::Object(land), 4)],
        })
        .expect("the restored save commits its division");
    drive(&mut resumed, land, land, None);
    let fact = declared_object_fact(resumed.state(), spell);
    assert_eq!(
        fact_object_id(&fact),
        land,
        "the resumed division save names the announced target"
    );
    assert!(
        fact_core_types(&fact).contains(&CoreType::Creature),
        "the resumed division save keeps the announcement-time creature: {:?}",
        fact_core_types(&fact)
    );

    // (b) A pre-latch pack of the same park — every latch stripped, pins still
    // current — is recovered at admission and records the same facts.
    let mut legacy = persisted_json(runner.state());
    assert!(
        strip_admission_latches(&mut legacy) > 0,
        "reach: the fixture really carried a latch to strip"
    );
    let recovered_state = admitted(legacy).map_err(|error| {
        format!("a division park that still proves its announcement is recovered: {error:?}")
    });
    let recovered_state =
        recovered_state.expect("reach: the recoverable division park is admitted");
    let mut resumed = GameRunner::from_state(recovered_state);
    resumed
        .act(GameAction::DistributeAmong {
            distribution: vec![(TargetRef::Object(land), 4)],
        })
        .expect("the recovered save commits its division");
    drive(&mut resumed, land, land, None);
    let fact = declared_object_fact(resumed.state(), spell);
    assert!(
        fact_core_types(&fact).contains(&CoreType::Creature),
        "the recovered division save keeps the announcement-time creature: {:?}",
        fact_core_types(&fact)
    );
}
