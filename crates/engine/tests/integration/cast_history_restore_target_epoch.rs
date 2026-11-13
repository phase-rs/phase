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
//! A fourth pack covers the case that divides a latch from a live read. The
//! **mid-payment characteristics change** pack is the same kind of cast parked
//! later, after CR 601.2g mana payment changed the announced target's
//! characteristics without moving it (CR 613.1 layers 4/6/7): Lightning Bolt
//! announced against Heliod, Sun-Crowned at five devotion, then Savannah Lions
//! sacrificed to Ashnod's Altar during the payment window, which drops devotion
//! to four so Heliod's own static stops making it a creature. Its incarnation pin
//! still matches — nothing changed zones — so identity alone cannot tell the
//! announcement from the post-payment state, and the stripped shape is refused.
//!
//! The distinction between "not declared yet" and "declared, unlatched, and
//! unrecoverable" is carried by the cast's own declared-target record and the
//! announcement latch, not by a new flag. An incarnation pin is NOT enough: the
//! pin proves which OBJECT was announced (CR 400.7), while an object's
//! characteristics are derived continuously from its printed values plus every
//! applicable continuous effect (CR 613.1), so a live read at restore time can
//! describe a different creature-shaped thing than the caster announced. The
//! guard therefore refuses every declared-but-unlatched park rather than
//! manufacturing facts from the board.
//!
//! A fifth pair covers the ZONE half of a declared requirement. Lightning Bolt is
//! announced at Savannah Lions and the Lions is sacrificed to Ashnod's Altar
//! during the payment window, so the target leaves the battlefield and its pin is
//! stale: the live zone comparison is inapplicable and the recorded announcement
//! is the only authority left. "Any target" is a creature, player, planeswalker
//! or battle, and the permanents among those are permanents (CR 115.4 +
//! CR 109.2), so the recorded requirement names the battlefield: the honest save
//! restores, and a save that restates the fact's zone as the graveyard — the
//! right TYPE in the wrong ZONE, which the live path never offered — is refused.

use crate::support::shared_card_db;
use engine::ai_support::legal_actions_full;
use engine::game::devotion::count_devotion;
use engine::game::game_object::{AttachTarget, ObjectIncarnationRefExt};
use engine::game::mana_abilities::is_mana_ability;
use engine::game::printed_cards::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    ActivationTargetFact, AnnouncedTargetFactConflict, AnnouncedTargetZone, CastPaymentMode,
    GameState, PayCostKind, PersistedGameState, PersistedRestoreError, SpellCastRecord, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
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

/// CR 601.2i: has `spell` become cast yet? The spell is on the stack from
/// CR 601.2a, so the stack entry cannot answer this — only the ledger can.
fn has_cast_record(state: &GameState, spell: ObjectId) -> bool {
    state
        .spells_cast_this_turn_by_player
        .values()
        .flat_map(|records| records.iter())
        .any(|record| record.spell_object_id == Some(spell))
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
/// a save written before the latch existed has.
///
/// The `PayCost` park needs the whole list rather than one field:
/// `pay_additional_cost_with_source` parks the cast on
/// `WaitingFor::PayCost`'s `CostResume::Spell` and does not set
/// `state.pending_cast`, so a strip scoped to `state.pending_cast` finds nothing
/// to strip there and tests nothing.
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

/// CR 601.2c + CR 115.1 + CR 400.7: the pre-return positive retained by the
/// narrowing. Same Devour in Flames fixture, parked one prompt earlier at the
/// return-a-land `PayCost` prompt with the target declared and no cost paid yet.
///
/// Nothing is owed here beyond what the engine already wrote: the park carries
/// the announced-target latch taken at the declaration boundary, so the save is
/// admitted and the RESUMED SAVE writes the announced creature. That latch is the
/// point — the animated Factory's incarnation pin is still current at this park
/// too, yet a current pin is exactly what a post-payment park also has (see
/// `a_mid_payment_characteristics_change_without_a_latch_is_refused`), so the pin
/// proves identity and never characteristics. What makes this park recoverable is
/// the latch, not the live board.
#[test]
fn a_return_cost_park_keeps_its_announced_target_through_resume() {
    let (runner, spell, land) = devour_parked_at_return_cost();

    // Reach guard: the announced target's incarnation pin is CURRENT at this park.
    // It is recorded here because it is emphatically NOT the reason the park is
    // admitted — the latch below is.
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
        "reach guard: the announced target has not moved yet"
    );

    // Reach guard: the park the engine writes carries the announcement latch, and
    // it describes the animated creature.
    let latched = parked
        .committed_target_facts
        .as_deref()
        .expect("reach guard: the pre-return park carries its announced-target latch");
    let latched_fact = latched
        .iter()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == land))
        .unwrap_or_else(|| {
            panic!("reach guard: the latch names the announced Factory; got {latched:?}")
        });
    assert!(
        fact_core_types(latched_fact).contains(&CoreType::Creature),
        "reach guard: the latch recorded the animated creature: {:?}",
        fact_core_types(latched_fact)
    );

    let restored_state = admitted(persisted_json(runner.state())).map_err(|error| {
        format!("a return-cost park carrying its announcement must be admitted: {error:?}")
    });
    let restored_state = restored_state.expect("reach: the latched park is admitted");

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
        "the resumed save records the announcement-time creature the caster announced, not the \
         returned card's printed type line: {:?}",
        fact_core_types(&fact)
    );
}

/// CR 601.2c + CR 115.1 + CR 400.7 + CR 613.1: the same pre-return park with the
/// latch stripped — the shape a save written before the latch existed has.
///
/// It is refused, and the narrowing is deliberate. Before this guard was narrowed,
/// a still-current incarnation pin was taken as proof and the live object was read
/// on the theory that nothing had been paid yet. That inference is about engine
/// flow, not about the save: a park this shape proves only that the target object
/// still exists in the same incarnation, and an object's characteristics are
/// recomputed continuously (CR 613.1). A declaration without its latch has no
/// record of what CR 601.2c announced, so the honest outcome is refusal rather
/// than a fabricated announcement.
#[test]
fn a_return_cost_park_without_the_latch_is_refused() {
    let (runner, spell, _land) = devour_parked_at_return_cost();

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

    let refused = admitted(legacy);
    assert!(
        matches!(
            refused,
            Err(PersistedRestoreError::UnknownPendingTargetFacts {
                spell_object_id: Some(id),
            }) if id == spell
        ),
        "a declared park without its announcement latch is refused, naming the spell: {:?}",
        refused.err()
    );
}

/// Oracle text for the mid-payment witness, read from the checked-in
/// `tests/fixtures/integration_cards.json.gz` (a strict subset of the
/// Scryfall/MTGJSON export the client ships):
///
/// * **Lightning Bolt** — `{R}`, Instant — "Lightning Bolt deals 3 damage to any
///   target."
/// * **Heliod, Sun-Crowned** — `{2}{W}`, Legendary Enchantment Creature — God,
///   5/5, Indestructible — "As long as your devotion to white is less than five,
///   Heliod isn't a creature. / Whenever you gain life, put a +1/+1 counter on
///   target creature or enchantment you control. / {1}{W}: Another target creature
///   gains lifelink until end of turn."
/// * **Benalish Marshal** — `{W}{W}{W}`, 3/3 Human Knight — "Other creatures you
///   control get +1/+1."
/// * **Savannah Lions** — `{W}`, 2/1 Cat.
/// * **Ashnod's Altar** — `{3}`, Artifact — "Sacrifice a creature: Add {C}{C}."
struct MidPaymentBoard {
    runner: GameRunner,
    spell: ObjectId,
    heliod: ObjectId,
    mountain: ObjectId,
}

/// CR 613.1d: layer 4 is what decides an object's card types, so "is this a
/// creature right now" is a live layer reading and not a printed property.
fn is_creature(state: &GameState, object: ObjectId) -> bool {
    state.objects[&object]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

/// Tap `land` for mana through the action the manual payment window offers.
fn tap_land_for_mana(runner: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the payment window offers the land's mana ability");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("the land taps for its mana");
}

/// CR 601.2c + CR 613.1d + CR 400.7: cast Lightning Bolt targeting Heliod,
/// Sun-Crowned as a creature at five devotion (Heliod + Benalish Marshal +
/// Savannah Lions all with white pips in their mana costs), then pay CR 601.2g
/// mana by sacrificing the Lions to Ashnod's Altar. Devotion drops to four, so
/// Heliod's own static stops applying a creature type in layer 4 — the announced
/// target keeps its storage id and its incarnation, and stops being the creature
/// that was announced.
///
/// Only two colorless mana are produced, so `{R}` is still owed and the cast
/// stays parked on the ordinary manual `ManaPayment` prompt: a save taken here is
/// mid-payment, with a characteristics change already behind it and the payment
/// boundary already crossed.
fn bolt_announced_at_a_creature_heliod_mid_payment() -> MidPaymentBoard {
    let db = shared_card_db().expect("the checked-in card fixture contains these cards");
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let spell = sc.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let heliod = sc.add_real_card(P0, "Heliod, Sun-Crowned", Zone::Battlefield, db);
    sc.add_real_card(P0, "Benalish Marshal", Zone::Battlefield, db);
    let lion = sc.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    let altar = sc.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let mountain = sc.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    let mut runner = sc.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    // CR 700.5: three white permanents' mana symbols put devotion at exactly five,
    // so Heliod is a creature — and a legal Bolt target — at announcement.
    assert_eq!(
        count_devotion(runner.state(), P0, &[ManaColor::White]),
        5,
        "reach guard: devotion to white is five at announcement"
    );
    assert!(
        is_creature(runner.state(), heliod),
        "reach guard: Heliod is a creature at announcement"
    );

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the cast is announced");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: the cast asks for its target, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(heliod)],
        })
        .expect("Heliod is a legal target for Lightning Bolt");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the cast parks on the manual mana payment, got {:?}",
        runner.state().waiting_for
    );

    // CR 601.2g: mana abilities are activated before costs are paid, and the
    // Altar's own cost moves a white permanent off the battlefield.
    let altar_index = runner.state().objects[&altar]
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("Ashnod's Altar has its mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: altar_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Altar's mana ability begins");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "reach guard: the Altar asks for the creature it sacrifices, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards { cards: vec![lion] })
        .expect("Savannah Lions pays the Altar's cost");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: only {{C}}{{C}} were produced, so the red payment is still open and the \
         cast is still parked on the payment window; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        count_devotion(runner.state(), P0, &[ManaColor::White]),
        4,
        "reach guard: the sacrifice dropped devotion to four"
    );
    assert!(
        !is_creature(runner.state(), heliod),
        "anti-vacuity: Heliod's own static has removed its creature type, so a live read here \
         describes an enchantment and not the announced creature"
    );

    // CR 400.7: nothing changed zones, so the announcement pin still matches and
    // says nothing about the characteristics change above.
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
        pin.is_current(runner.state()),
        "reach guard: the announced target never changed zones, so its pin still matches"
    );

    MidPaymentBoard {
        runner,
        spell,
        heliod,
        mountain,
    }
}

/// CR 601.2c + CR 115.1 + CR 601.2i: the mid-payment positive. The park the
/// engine writes carries the announcement latch taken at the declaration
/// boundary — before the Altar's sacrifice — so the save is admitted and the
/// RESUMED SAVE records the creature the caster announced, even though the object
/// is no longer a creature by the time the cast commits.
#[test]
fn a_mid_payment_characteristics_change_keeps_the_announcement_through_resume() {
    let MidPaymentBoard {
        runner,
        spell,
        heliod,
        mountain,
    } = bolt_announced_at_a_creature_heliod_mid_payment();

    // Reach guard: the announcement latch was taken before the sacrifice and holds
    // the creature Heliod was at CR 601.2c.
    let parked = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach guard: the ManaPayment prompt owns the cast");
    let latched = parked
        .committed_target_facts
        .as_deref()
        .expect("reach guard: the engine-written park carries its announced-target latch");
    let latched_fact = latched
        .iter()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == heliod))
        .unwrap_or_else(|| {
            panic!("reach guard: the latch names Heliod as the announced target; got {latched:?}")
        });
    assert!(
        fact_core_types(latched_fact).contains(&CoreType::Creature),
        "reach guard: the latch holds the announcement-time creature: {:?}",
        fact_core_types(latched_fact)
    );

    let restored_state = admitted(persisted_json(runner.state())).map_err(|error| {
        format!("a mid-payment park carrying its announcement must be admitted: {error:?}")
    });
    let restored_state = restored_state.expect("reach: the latched park is admitted");

    // The positive continuation resumes the RESTORED state and pays the red mana.
    let mut resumed = GameRunner::from_state(restored_state);
    assert!(
        matches!(resumed.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the restored state still owes the mana payment, got {:?}",
        resumed.state().waiting_for
    );
    tap_land_for_mana(&mut resumed, mountain);
    for _ in 0..8 {
        if has_cast_record(resumed.state(), spell) {
            break;
        }
        resumed
            .act(GameAction::PassPriority)
            .expect("the restored save pays its total cost");
    }
    assert!(
        has_cast_record(resumed.state(), spell),
        "reach guard: the resumed save completed the cast, got {:?}",
        resumed.state().waiting_for
    );

    let fact = declared_object_fact(resumed.state(), spell);
    assert_eq!(
        fact_object_id(&fact),
        heliod,
        "the resumed save's record names the announced target"
    );
    assert!(
        fact_core_types(&fact).contains(&CoreType::Creature),
        "the resumed save records the announcement-time creature the caster announced, not \
         Heliod's post-sacrifice type line: {:?}",
        fact_core_types(&fact)
    );
}

/// CR 601.2c + CR 613.1 + CR 400.7: the same mid-payment pack with the latch
/// stripped — the shape a save written before the latch existed has.
///
/// This is the case that pins the narrowing. The declared target's incarnation pin
/// is still current, so the identity-only check admits the pack and reads the live
/// object; but the live object is an enchantment at four devotion, so the recovered
/// "announcement" would be post-payment characteristics wearing the announcement's
/// name. The announcement is simply not in the save, so the save is refused.
#[test]
fn a_mid_payment_characteristics_change_without_a_latch_is_refused() {
    let MidPaymentBoard { runner, spell, .. } = bolt_announced_at_a_creature_heliod_mid_payment();

    let mut legacy = persisted_json(runner.state());
    assert!(
        strip_admission_latches(&mut legacy) > 0,
        "reach: the fixture really carried a latch to strip"
    );

    let refused = admitted(legacy);
    match refused {
        Ok(admitted) => {
            // A regression to a live read shows up here as an admission wearing the
            // announcement's name, so name the fabricated facts in the failure.
            let stamped = admitted
                .pending_cast
                .as_deref()
                .and_then(|pending| pending.committed_target_facts.as_deref());
            panic!(
                "a mid-payment park that cannot prove its announcement must be refused; it was \
                 admitted and stamped with {stamped:?}"
            );
        }
        Err(PersistedRestoreError::UnknownPendingTargetFacts {
            spell_object_id: Some(id),
        }) if id == spell => {}
        Err(other) => panic!("the mid-payment park was refused for the wrong reason: {other:?}"),
    }
}

/// CR 601.2c: Printed **Doom Blade** is "{1}{B} — Destroy target creature."
/// (Scryfall, oracle text verified). Its declared filter names a TYPE and no
/// ZONE — a bare "target creature" — which leaves the LIVE zone comparison as
/// the only check that can judge a restated zone for this slot. A requirement
/// that DOES name a zone is answered by the recorded requirement first, with
/// `SnapshotContradictsDeclaredFilter`: a damage "any target" names the
/// battlefield (creature, player, planeswalker or battle — CR 115.4 + CR 109.2),
/// and Reanimate's "creature card from a graveyard" names the graveyard
/// (CR 109.2a).
const DOOM_BLADE: &str = "Destroy target creature.";

struct ZoneBoard {
    runner: GameRunner,
    spell: ObjectId,
    target: ObjectId,
}

/// CR 601.2c: announce Doom Blade at P1's creature with `CastPaymentMode::Manual`
/// so the cast parks at `WaitingFor::ManaPayment` holding its announced-target
/// latch — the state a save at the payment boundary captures. The Bears is the
/// only legal target, so the engine announces it without a prompt (the same
/// sole-target shape the Reanimate witness relies on).
fn doom_blade_parked_at_manual_payment() -> ZoneBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, DOOM_BLADE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the manual Doom Blade cast is announced");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the sole legal target is announced and the cast parks on the manual mana \
         payment, got {:?}",
        runner.state().waiting_for
    );
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach-guard: the ManaPayment prompt owns the cast");
    assert_eq!(
        pending.ability.targets,
        vec![TargetRef::Object(target)],
        "reach-guard: the park announced the only legal creature"
    );
    ZoneBoard {
        runner,
        spell,
        target,
    }
}

/// CR 601.2c + CR 400.7: the same-TARGET inconsistency control. The object id, the
/// announcement-time characteristics and every other field of the latch are the HONEST ones;
/// only the stated ZONE is restated. An announcement pin that is still current proves the
/// object never changed zones since CR 601.2c, so a fact claiming another zone was not
/// captured at this announcement — and `record_spell_cast_from_zone` would publish it as the
/// CR 601.2i history row. The honest pack is admitted (the control above), this one is not.
///
/// The characteristics are deliberately left untouched: they are the announcement's record
/// and are NOT re-derivable, so the guard binds the zone and never compares the live object.
///
/// The declared slot names no zone ("target creature"), so the RECORDED requirement has no
/// zone axis to answer and the LIVE comparison against the still-current pin is what refuses
/// the restated zone.
#[test]
fn a_latch_stating_a_zone_the_announcement_pin_forbids_is_refused() {
    let board = doom_blade_parked_at_manual_payment();

    // The control: this exact pack, unmodified, is admitted.
    admitted(persisted_json(board.runner.state()))
        .expect("reach-guard: the honest pack is admitted");

    // Same id, same announcement-time characteristics, only the zone restated.
    let mut state = board.runner.state().clone();
    let parked = state
        .pending_cast
        .as_deref_mut()
        .expect("reach-guard: the ManaPayment prompt owns the cast");
    let latched = parked
        .committed_target_facts
        .as_deref_mut()
        .expect("reach-guard: the engine-written park carries its announced-target latch");
    let fact = latched
        .iter_mut()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == board.target))
        .expect("reach-guard: the latch names the Bears");
    let ActivationTargetFact::Object { zone, .. } = fact else {
        unreachable!("filtered to the object fact above");
    };
    assert_eq!(
        *zone,
        AnnouncedTargetZone::Known(Zone::Battlefield),
        "reach-guard: the honest fact states the battlefield it was announced on"
    );
    *zone = AnnouncedTargetZone::Known(Zone::Graveyard);

    let refused = PersistedGameState::capture(state).into_game_state();
    match refused {
        Err(PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason:
                AnnouncedTargetFactConflict::ZoneContradictsAnnouncement {
                    target: TargetRef::Object(target),
                    stated,
                    pinned,
                },
        }) => {
            assert_eq!(
                spell_object_id, board.spell,
                "the refusal names the cast it is about"
            );
            assert_eq!(
                target, board.target,
                "and the target whose zone it contradicts"
            );
            assert_eq!(
                stated,
                AnnouncedTargetZone::Known(Zone::Graveyard),
                "and the zone the latch states"
            );
            assert_eq!(
                pinned,
                Zone::Battlefield,
                "against the battlefield its still-current pin proves"
            );
        }
        other => panic!(
            "CR 601.2c + CR 400.7: a latch stating a zone its announcement pin forbids must be \
             refused, got {other:?}"
        ),
    }
}

/// CR 601.2c + CR 400.7: the zone check is not optional-by-payload. Dropping the
/// announcement pins must not turn it off: an ABSENT pin means the save recorded no
/// departure, so the object is still in the zone it was announced in — which makes a
/// restated zone a contradiction, not a stale record.
///
/// The honest pinless pack is admitted (its stated zone IS the live zone, because nothing
/// moved the object); the same pack with the zone restated is refused even though the pins
/// that would have caught it earlier are gone.
///
/// The pinless honest pack is a state the ENGINE never writes — every latch it captures today
/// carries its pins, and a target that legitimately moved keeps a stale pin and is skipped. It
/// is exercised here because a save can reach that shape, and the guard must not treat the
/// missing evidence as permission. If a future writer ever emits a Known announcement zone
/// with no pin for an object that HAS moved, this comparison would refuse it as a
/// contradiction; that writer must also record the pin, which is what makes the zone
/// checkable at all.
///
/// As in the pinned twin above, the declared slot names no zone ("target creature"), so the
/// LIVE comparison is the check under test here.
#[test]
fn a_pinless_latch_still_cannot_state_a_zone_the_object_never_left() {
    let board = doom_blade_parked_at_manual_payment();

    // The honest pinless control: same latch, pins dropped, zone untouched — ADMITTED.
    {
        let mut state = board.runner.state().clone();
        let parked = state
            .pending_cast
            .as_deref_mut()
            .expect("reach-guard: the ManaPayment prompt owns the cast");
        parked.ability.target_pins.clear();
        admitted(persisted_json(&state))
            .expect("CR 601.2c: an honest latch is admitted even without its pins");
    }

    // Same pack, pins dropped AND the zone restated — REFUSED.
    let mut state = board.runner.state().clone();
    let parked = state
        .pending_cast
        .as_deref_mut()
        .expect("reach-guard: the ManaPayment prompt owns the cast");
    parked.ability.target_pins.clear();
    let latched = parked
        .committed_target_facts
        .as_deref_mut()
        .expect("reach-guard: the engine-written park carries its announced-target latch");
    let fact = latched
        .iter_mut()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == board.target))
        .expect("reach-guard: the latch names the Bears");
    let ActivationTargetFact::Object { zone, .. } = fact else {
        unreachable!("filtered to the object fact above");
    };
    *zone = AnnouncedTargetZone::Known(Zone::Graveyard);

    match PersistedGameState::capture(state).into_game_state() {
        Err(PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason:
                AnnouncedTargetFactConflict::ZoneContradictsAnnouncement {
                    target: TargetRef::Object(target),
                    pinned,
                    ..
                },
        }) => {
            assert_eq!(spell_object_id, board.spell, "the refusal names the cast");
            assert_eq!(target, board.target, "and the target");
            assert_eq!(pinned, Zone::Battlefield, "against the zone it never left");
        }
        other => panic!(
            "CR 601.2c + CR 400.7: dropping the pins must not disable the zone check, got {other:?}"
        ),
    }
}

/// CR 601.2c + CR 115.4: "any target" is NOT unrestricted. Lightning Bolt's "deals 3 damage to
/// any target" admits "creatures, players, planeswalkers, or battles" — and nothing else — so an
/// enchantment-only snapshot is an announcement the spell could never have made and must be
/// REFUSED. The damage domain is resolved through
/// `ability_utils::declared_slot_announcement_filter`, the same authority that builds the
/// slot's legal targets.
///
/// The honest twin is `a_mid_payment_characteristics_change_keeps_the_announcement_through_resume`:
/// Heliod's recorded CREATURE snapshot still satisfies the domain after its live type drifts,
/// because the check reads the snapshot and never the live object.
#[test]
fn a_damage_any_target_slot_refuses_a_restated_type() {
    let MidPaymentBoard {
        runner,
        spell,
        heliod,
        ..
    } = bolt_announced_at_a_creature_heliod_mid_payment();

    let mut state = runner.state().clone();
    let parked = state
        .pending_cast
        .as_deref_mut()
        .expect("reach-guard: the ManaPayment prompt owns the cast");
    let latched = parked
        .committed_target_facts
        .as_deref_mut()
        .expect("reach-guard: the engine-written park carries its announced-target latch");
    let fact = latched
        .iter_mut()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == heliod))
        .expect("reach-guard: the latch names Heliod");
    let ActivationTargetFact::Object { lki, .. } = fact else {
        unreachable!("filtered to the object fact above");
    };
    lki.card_types.retain(|kind| *kind != CoreType::Creature);

    match PersistedGameState::capture(state).into_game_state() {
        Err(PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason:
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter {
                    target: TargetRef::Object(refused),
                },
        }) => {
            assert_eq!(
                spell_object_id, spell,
                "the refusal names the cast it is about"
            );
            assert_eq!(
                refused, heliod,
                "and the target whose snapshot was rewritten"
            );
        }
        other => panic!(
            "CR 115.4: a damage \"any target\" slot admits only creatures, players, \
             planeswalkers and battles; an enchantment-only snapshot must be refused, got {other:?}"
        ),
    }
}

/// CR 601.2c + CR 115.4: the same target id, the pin still current, the zone still the
/// battlefield — only the recorded TYPE rewritten to what the object looks like after the
/// effects that made it a legal target have gone. Identity, zone and pin all still check
/// out, so only the declaring slot's own filter can catch it.
///
/// Printed **Doom Blade** is `{1}{B}` — "Destroy target creature." — so its declared slot
/// constrains the type. Heliod, Sun-Crowned is that creature at five devotion; the forged
/// snapshot drops `Creature`, which the slot's filter never admitted in either direction.
/// The honest twin is admitted: the Lightning Bolt witness above is the same mechanism on an
/// UNRESTRICTED ("any target") slot, which an enchantment-only snapshot legitimately
/// satisfies — that case is the positive control that the check does not over-refuse.
#[test]
fn a_type_constrained_slot_refuses_a_snapshot_restating_the_post_payment_type() {
    let db = shared_card_db().expect("the checked-in card fixture contains these cards");
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let spell = sc.add_real_card(P0, "Doom Blade", Zone::Hand, db);
    let heliod = sc.add_real_card(P0, "Heliod, Sun-Crowned", Zone::Battlefield, db);
    sc.add_real_card(P0, "Benalish Marshal", Zone::Battlefield, db);
    sc.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    let mut runner = sc.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    assert!(
        is_creature(runner.state(), heliod),
        "reach-guard: Heliod is a creature at announcement, so Doom Blade may target it"
    );
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Doom Blade is announced");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach-guard: the cast asks for its target, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(heliod)],
        })
        .expect("Heliod is a legal target for Doom Blade");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach-guard: the cast parks on the manual mana payment, got {:?}",
        runner.state().waiting_for
    );

    // The honest twin restores.
    admitted(persisted_json(runner.state())).expect("reach-guard: the honest pack is admitted");

    let mut state = runner.state().clone();
    let parked = state
        .pending_cast
        .as_deref_mut()
        .expect("reach-guard: the ManaPayment prompt owns the cast");
    let latched = parked
        .committed_target_facts
        .as_deref_mut()
        .expect("reach-guard: the engine-written park carries its announced-target latch");
    let fact = latched
        .iter_mut()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == heliod))
        .expect("reach-guard: the latch names Heliod");
    let ActivationTargetFact::Object { id, zone, lki, .. } = fact else {
        unreachable!("filtered to the object fact above");
    };
    let target = *id;
    assert_eq!(
        *zone,
        AnnouncedTargetZone::Known(Zone::Battlefield),
        "reach-guard: the honest fact says battlefield, and this control changes only the type"
    );
    assert!(
        lki.card_types.contains(&CoreType::Creature),
        "reach-guard: the honest snapshot records the creature the slot's filter required"
    );
    // CR 613.1d: the post-payment type line — an enchantment, which "target creature" never
    // admitted.
    lki.card_types.retain(|kind| *kind != CoreType::Creature);

    match PersistedGameState::capture(state).into_game_state() {
        Err(PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason:
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter {
                    target: TargetRef::Object(refused),
                },
        }) => {
            assert_eq!(
                spell_object_id, spell,
                "the refusal names the cast it is about"
            );
            assert_eq!(
                refused, target,
                "and the target whose snapshot was rewritten"
            );
        }
        other => panic!(
            "CR 601.2c + CR 115.4: a type-constrained slot must refuse a snapshot restating the \
             post-payment type, got {other:?}"
        ),
    }
}

/// CR 115.4: "any target" on a damage effect names a creature, player,
/// planeswalker or battle — and three of those four are PERMANENTS, which exist
/// only on the battlefield. The live domain therefore searches battlefield
/// objects (`targeting::find_legal_targets_for_ability`, no explicit zone), so
/// an announcement that names a creature CARD in a graveyard is one the spell
/// could never have made: live selection never offers it.
///
/// The recorded requirement must say the same thing. Its snapshot answers the
/// type axis, and only the fact's captured zone can answer WHERE the
/// announcement was made — the live zone comparison cannot, because the target
/// moved and its incarnation pin is stale (CR 400.7). That is exactly the board
/// below: Lightning Bolt is announced at Savannah Lions, a legal creature on the
/// battlefield, and the Lions is then sacrificed to Ashnod's Altar for `{C}{C}`
/// during CR 601.2g mana payment, leaving `{R}` still owed.
struct TargetLeftBoard {
    runner: GameRunner,
    spell: ObjectId,
    lions: ObjectId,
}

fn bolt_announced_at_a_creature_that_left_mid_payment() -> TargetLeftBoard {
    let db = shared_card_db().expect("the checked-in card fixture contains these cards");
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let spell = sc.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let lions = sc.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    let altar = sc.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let mut runner = sc.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Lightning Bolt is announced");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: the cast asks for its target, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(lions)],
        })
        .expect("Savannah Lions is a legal battlefield creature, so Lightning Bolt may target it");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the cast parks on the manual mana payment, got {:?}",
        runner.state().waiting_for
    );

    // CR 601.2g: the Altar's mana ability sacrifices the ANNOUNCED TARGET itself,
    // so the creature Bolt named leaves the battlefield before the cast has paid.
    // Only `{C}{C}` is produced, so `{R}` stays owed and the park survives.
    let altar_index = runner.state().objects[&altar]
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("Ashnod's Altar has its mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: altar_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Altar's mana ability begins");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "reach guard: the Altar asks for the creature it sacrifices, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards { cards: vec![lions] })
        .expect("the announced target pays the Altar's cost");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: only {{C}}{{C}} were produced, so the red payment is still open, got {:?}",
        runner.state().waiting_for
    );

    TargetLeftBoard {
        runner,
        spell,
        lions,
    }
}

/// Reach guards shared by both directions: the announced creature LEFT the
/// battlefield, its pin is stale (so the live zone comparison is inapplicable),
/// and the recorded fact still states the battlefield it was announced on.
fn assert_target_left_reach_guards(board: &TargetLeftBoard) {
    let state = board.runner.state();
    assert_eq!(
        state.objects[&board.lions].zone,
        Zone::Graveyard,
        "reach guard: the announced creature really left the battlefield"
    );
    let pending = state
        .pending_cast
        .as_deref()
        .expect("reach guard: the ManaPayment prompt owns the cast");
    assert_eq!(
        pending.ability.targets,
        vec![TargetRef::Object(board.lions)],
        "reach guard: the park still names the creature it announced"
    );
    let pin = pending
        .ability
        .target_pins
        .first()
        .copied()
        .flatten()
        .expect("reach guard: the announced target carries an incarnation pin");
    assert!(
        !pin.is_current(state),
        "CR 400.7: the announced creature changed zones, so its pin is STALE and the live zone \
         comparison cannot answer where it was announced"
    );
    let facts = pending
        .committed_target_facts
        .as_deref()
        .expect("reach guard: the engine-written park carries its announced-target latch");
    let [ActivationTargetFact::Object { id, zone, lki, .. }] = facts else {
        panic!("reach guard: the latch is one object fact, got {facts:?}");
    };
    assert_eq!(*id, board.lions, "reach guard: the latch names the Lions");
    assert_eq!(
        *zone,
        AnnouncedTargetZone::Known(Zone::Battlefield),
        "CR 601.2c: the honest fact states the battlefield it was announced on"
    );
    assert!(
        lki.card_types.contains(&CoreType::Creature),
        "reach guard: the recorded snapshot says the announced target was a creature"
    );
}

/// WITNESS (honest direction): the recorded announcement named a legal creature
/// on the battlefield, so the park restores even though the creature has since
/// moved off it. CR 601.2c validates the declaration the caster made; the
/// departure is a later event (CR 608.2b decides the spell's fate at
/// resolution), and the stale pin must not turn the honest record into a
/// contradiction.
///
/// This is the positive control for the battlefield requirement: the requirement
/// is read from the RECORDED announcement, so a target that has left the
/// battlefield still carries the zone the caster announced it in.
#[test]
fn a_damage_any_target_announced_at_a_creature_that_left_still_restores() {
    let board = bolt_announced_at_a_creature_that_left_mid_payment();
    assert_target_left_reach_guards(&board);

    let restored = admitted(persisted_json(board.runner.state())).unwrap_or_else(|error| {
        panic!(
            "CR 601.2c + CR 115.4: the recorded announcement named a legal creature on the \
             battlefield, so this save is admitted; the creature's later departure is not a \
             contradiction — got {error:?}"
        )
    });
    let pending = restored
        .pending_cast
        .as_deref()
        .expect("the restored park keeps its cast");
    assert_eq!(
        pending.ability.targets,
        vec![TargetRef::Object(board.lions)],
        "the restored cast still names the creature it announced"
    );
}

/// WITNESS (refusal direction): the same honest park with the fact's stated zone
/// restated as the graveyard. The snapshot still says CREATURE — the type is
/// right and only the zone is wrong — which is precisely the announcement
/// CR 115.4 forbids: live damage selection searches battlefield objects, so a
/// creature in a graveyard is not an "any target" the spell could have named.
///
/// Nothing else catches it: the target really did move, so its pin is stale and
/// the live zone comparison is skipped (CR 400.7). The declaring slot's recorded
/// requirement is the only remaining authority, and it must carry the battlefield
/// with the type set.
#[test]
fn a_damage_any_target_fact_naming_its_creature_outside_the_battlefield_is_refused() {
    let board = bolt_announced_at_a_creature_that_left_mid_payment();
    assert_target_left_reach_guards(&board);

    // The control: the untouched park is admitted.
    admitted(persisted_json(board.runner.state()))
        .expect("reach-guard: the honest park with the factual battlefield zone is admitted");

    let mut state = board.runner.state().clone();
    let parked = state
        .pending_cast
        .as_deref_mut()
        .expect("reach-guard: the ManaPayment prompt owns the cast");
    let latched = parked
        .committed_target_facts
        .as_deref_mut()
        .expect("reach-guard: the engine-written park carries its announced-target latch");
    let fact = latched
        .iter_mut()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == board.lions))
        .expect("reach-guard: the latch names the Lions");
    let ActivationTargetFact::Object { lki, zone, .. } = fact else {
        unreachable!("filtered to the object fact above");
    };
    assert!(
        lki.card_types.contains(&CoreType::Creature),
        "reach-guard: the restated announcement keeps the CORRECT type; only its zone changes"
    );
    *zone = AnnouncedTargetZone::Known(Zone::Graveyard);

    match PersistedGameState::capture(state).into_game_state() {
        Err(PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason:
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter {
                    target: TargetRef::Object(refused),
                },
        }) => {
            assert_eq!(
                spell_object_id, board.spell,
                "the refusal names the cast it is about"
            );
            assert_eq!(
                refused, board.lions,
                "and the target whose announcement contradicts its slot"
            );
        }
        other => panic!(
            "CR 115.4: a damage \"any target\" slot admits only battlefield creatures, players, \
             planeswalkers and battles; a creature recorded in a graveyard was never a legal \
             announcement, got {other:?}"
        ),
    }
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
    // current — cannot prove what CR 601.2c announced, so it is refused. A current
    // pin is identity only (CR 400.7); characteristics come from printed values
    // plus every continuous effect (CR 613.1), so a live read is not the
    // announcement.
    let mut legacy = persisted_json(runner.state());
    assert!(
        strip_admission_latches(&mut legacy) > 0,
        "reach: the fixture really carried a latch to strip"
    );
    let refused = admitted(legacy);
    assert!(
        matches!(
            refused,
            Err(PersistedRestoreError::UnknownPendingTargetFacts {
                spell_object_id: Some(id),
            }) if id == spell
        ),
        "a division park without its announcement is refused, naming the spell: {:?}",
        refused.err()
    );
}

/// Printed **Bushwhack** is `{G}` Sorcery — "Choose one — / • Search your library
/// for a basic land card, reveal it, put it into your hand, then shuffle. / •
/// Target creature you control fights target creature you don't control."
/// (MTGJSON AtomicCards, oracle text verified).
const BUSHWHACK: &str = "Choose one —\n\
    • Search your library for a basic land card, reveal it, put it into your hand, then shuffle.\n\
    • Target creature you control fights target creature you don't control.";

/// The two-fighter park: `own` is the SUBJECT slot's legal object, `theirs` the
/// TARGET slot's, and the untapped Forest pays the `{G}`, so the cast parks on the
/// ordinary manual payment prompt with both fighters announced and latched.
///
/// CR 601.2h: the `Manual` request the cast announces survives `handle_select_modes`
/// — the rebuilt pending cast carries `payment_mode` across the mode choice, the
/// same way it carries `base_cost` and `chosen_modes` — so the fixture needs no
/// restatement: the cast parks on the manual prompt it asked for, before the
/// auto-payer can settle `{G}` from the Forest.
struct BushwhackPark {
    runner: GameRunner,
    spell: ObjectId,
    own: ObjectId,
    theirs: ObjectId,
    forest: ObjectId,
}

fn bushwhack_announced_mid_payment() -> BushwhackPark {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Bushwhack", false, BUSHWHACK)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    // 2/4 against 3/3: each survives the other's damage, so both damage marks stay
    // readable after resolution. The spares keep each slot from auto-selecting the
    // only legal object, so the caster really announces both fighters.
    let own = sc.add_creature(P0, "Own fighter", 3, 3).id();
    let theirs = sc.add_creature(P1, "Opposing fighter", 2, 4).id();
    sc.add_creature(P0, "Own spare", 1, 1);
    sc.add_creature(P1, "Opposing spare", 1, 1);
    let forest = sc.add_land_from_oracle(P0, "Forest", "{T}: Add {G}.").id();
    let mut runner = sc.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the cast is announced");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ModeChoice { .. }),
        "reach guard: Bushwhack is a \"Choose one\" spell, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("the fight mode is chosen");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: the fight mode asks for its two fighters, got {:?}",
        runner.state().waiting_for
    );
    // CR 601.2c: positions are the declared slots — subject first, target second.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(own), TargetRef::Object(theirs)],
        })
        .expect("both announced fighters are legal for their own slots");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the cast parks on the manual mana payment, got {:?}",
        runner.state().waiting_for
    );
    BushwhackPark {
        runner,
        spell,
        own,
        theirs,
        forest,
    }
}

/// CR 601.2c: an appropriate object for EACH target the spell requires. Bushwhack's
/// two declared slots impose DIFFERENT filters — "creature you control" for the
/// subject, "creature you don't control" for the target — so a save whose recorded
/// facts satisfy those filters slot by slot is an announcement the spell really
/// made: it must be admitted, and the resumed cast must actually fight.
///
/// This is the witness for the per-slot recovery in
/// `ability_utils::declared_slot_announcement_filter`. Recovering one filter for the
/// whole effect (the `target` field's "you don't control") refuses the honest
/// friendly fact in slot 0 and this test fails at the restore boundary.
#[test]
fn a_bushwhack_park_keeps_each_fighters_own_filter_through_restore() {
    let BushwhackPark {
        runner,
        spell,
        own,
        theirs,
        forest,
    } = bushwhack_announced_mid_payment();

    // Reach guard: the engine-written latch records both fighters, subject first.
    let parked = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach guard: the ManaPayment prompt owns the cast");
    let latched = parked
        .committed_target_facts
        .as_deref()
        .expect("reach guard: the parked cast carries its announced-target latch");
    let latched_ids: Vec<ObjectId> = latched
        .iter()
        .filter_map(|fact| match fact {
            ActivationTargetFact::Object { id, .. } => Some(*id),
            ActivationTargetFact::Player(_) => None,
        })
        .collect();
    assert_eq!(
        latched_ids,
        vec![own, theirs],
        "CR 601.2c: the latch records the declared slots in order — subject, then target"
    );

    let restored_state = admitted(persisted_json(runner.state())).map_err(|error| {
        format!(
            "a Bushwhack park whose facts satisfy their own slots' filters must be admitted: \
             {error:?}"
        )
    });
    let restored_state =
        restored_state.expect("reach: the honest two-fighter announcement is admitted");

    // The resumed save pays the locked {G} and the fight resolves from the restored
    // state, not from the pre-save runner.
    let mut resumed = GameRunner::from_state(restored_state);
    assert!(
        matches!(resumed.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the restored state still owes the manual mana payment, got {:?}",
        resumed.state().waiting_for
    );
    tap_land_for_mana(&mut resumed, forest);
    resumed
        .act(GameAction::PassPriority)
        .expect("the restored save pays its locked cost");
    assert!(
        resumed.state().stack.iter().any(|entry| entry.id == spell),
        "reach guard: the resumed save put Bushwhack on the stack"
    );
    resumed.advance_until_stack_empty();
    assert!(
        resumed.state().stack.is_empty(),
        "reach guard: the resumed Bushwhack resolved"
    );
    // CR 701.14a: each fighter deals damage equal to its power to the other.
    assert_eq!(
        resumed.state().objects[&theirs].damage_marked,
        3,
        "the fought creature takes the subject's 3 power"
    );
    assert_eq!(
        resumed.state().objects[&own].damage_marked,
        2,
        "the fighting creature takes the 2 power of the creature it fought"
    );
}

/// CR 601.2c: the twin of the witness above with the two announced fighters SWAPPED
/// between the declared slots (and the latch swapped with them, so the save is
/// internally consistent). Slot 0 then records the opponent's creature, which only
/// slot 1's "creature you don't control" filter admits — an announcement Bushwhack
/// can never make, and it must be refused.
///
/// The refusal must name the SUBJECT slot's object: that is the per-slot recovery
/// deciding. Recovering one filter for the whole effect judges slot 0 with "you
/// don't control" (so the opponent's fact passes there) and refuses slot 1's
/// friendly fact instead — the save is still refused, but with the wrong reason and
/// the wrong target, and this test fails on the targeted match.
#[test]
fn a_bushwhack_park_with_its_two_fighters_swapped_is_refused() {
    let BushwhackPark {
        runner,
        spell,
        own: _,
        theirs,
        forest: _,
    } = bushwhack_announced_mid_payment();

    let mut state = runner.state().clone();
    {
        let parked = state
            .pending_cast
            .as_deref_mut()
            .expect("reach-guard: the ManaPayment prompt owns the cast");
        assert_eq!(
            parked.ability.targets.len(),
            2,
            "reach-guard: the declaration really holds both fighters"
        );
        parked.ability.targets.swap(0, 1);
        if parked.ability.target_pins.len() >= 2 {
            parked.ability.target_pins.swap(0, 1);
        }
        let latched = parked
            .committed_target_facts
            .as_deref_mut()
            .expect("reach-guard: the engine-written park carries its announced-target latch");
        latched.swap(0, 1);
    }

    match PersistedGameState::capture(state).into_game_state() {
        Err(PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason:
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter {
                    target: TargetRef::Object(refused),
                },
        }) => {
            assert_eq!(
                spell_object_id, spell,
                "the refusal names the cast it is about"
            );
            assert_eq!(
                refused, theirs,
                "CR 601.2c: slot 0 is the SUBJECT — \"creature you control\" is the filter \
                 the opponent's fighter fails, not slot 1's"
            );
        }
        other => panic!(
            "CR 601.2c: a save that hands the subject slot a creature you don't control must be \
             refused, got {other:?}"
        ),
    }
}

/// Printed **Magnetic Theft** is `{R}` Instant — "Attach target Equipment to target
/// creature. (Control of the Equipment doesn't change.)" (Scryfall `/cards/named?
/// exact=Magnetic+Theft`, oracle text verified).
const MAGNETIC_THEFT: &str =
    "Attach target Equipment to target creature. (Control of the Equipment doesn't change.)";

/// The two-operand `Attach` park: `equipment` is the ATTACHMENT slot's object (an
/// ordinary noncreature Equipment artifact), `creature` the HOST slot's, and the
/// untapped Mountain pays the `{R}`, so the cast parks on the ordinary manual
/// payment prompt with both operands announced and latched. The spare Equipment and
/// spare creature keep either slot from auto-selecting its only legal object, so the
/// caster really announces both.
struct MagneticTheftPark {
    runner: GameRunner,
    spell: ObjectId,
    equipment: ObjectId,
    creature: ObjectId,
    mountain: ObjectId,
}

fn magnetic_theft_announced_mid_payment() -> MagneticTheftPark {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Magnetic Theft", true, MAGNETIC_THEFT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    // CR 301.5: an Equipment is an artifact with the Equipment subtype — an ordinary
    // one is NOT a creature, which is what makes the host's "target creature" filter
    // the wrong filter for this slot.
    let equipment = sc
        .add_artifact_from_oracle(P0, "Sword of the Test", "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"])
        .id();
    sc.add_artifact_from_oracle(P0, "Spare Sword", "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"]);
    let creature = sc.add_creature(P0, "Bearer", 2, 2).id();
    sc.add_creature(P0, "Spare Bear", 1, 1);
    let mountain = sc
        .add_land_from_oracle(P0, "Mountain", "{T}: Add {R}.")
        .id();
    let mut runner = sc.build();

    cast(&mut runner, spell);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: the cast asks for its two operands, got {:?}",
        runner.state().waiting_for
    );
    // CR 601.2c + CR 115.1a: positions are the declared slots — attachment first,
    // host second.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(equipment), TargetRef::Object(creature)],
        })
        .expect("both announced operands are legal for their own slots");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the cast parks on the manual mana payment, got {:?}",
        runner.state().waiting_for
    );
    MagneticTheftPark {
        runner,
        spell,
        equipment,
        creature,
        mountain,
    }
}

/// CR 115.1a/c/d/e + CR 601.2c + CR 608.2d: an `Attach` instruction announces its
/// two operands with two DIFFERENT filters — "target Equipment" for the attachment,
/// "target creature" for the host — so a save whose recorded facts satisfy those
/// filters slot by slot is an announcement the spell really made: it must be
/// admitted, and the resumed cast must actually attach the Equipment.
///
/// This is the witness for the per-slot recovery in
/// `ability_utils::declared_slot_announcement_filter`. Recovering one filter for the
/// whole effect reads `Effect::target_filter()`'s Attach arm — the HOST's "target
/// creature" — for BOTH positions, so slot 0 is judged as a creature and the
/// untouched save of an ordinary noncreature Equipment is refused at the restore
/// boundary (`SnapshotContradictsDeclaredFilter` naming the Equipment).
#[test]
fn a_magnetic_theft_park_keeps_each_operands_own_filter_through_restore() {
    let MagneticTheftPark {
        runner,
        spell,
        equipment,
        creature,
        mountain,
    } = magnetic_theft_announced_mid_payment();

    // Reach guard: the engine-written latch records both operands, attachment first,
    // and the attachment really is a noncreature Equipment — the shape the shared
    // host filter cannot admit.
    let parked = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach guard: the ManaPayment prompt owns the cast");
    let latched = parked
        .committed_target_facts
        .as_deref()
        .expect("reach guard: the parked cast carries its announced-target latch");
    let latched_ids: Vec<ObjectId> = latched
        .iter()
        .filter_map(|fact| match fact {
            ActivationTargetFact::Object { id, .. } => Some(*id),
            ActivationTargetFact::Player(_) => None,
        })
        .collect();
    assert_eq!(
        latched_ids,
        vec![equipment, creature],
        "CR 601.2c: the latch records the declared slots in order — attachment, then host"
    );
    let equipment_types = runner.state().objects[&equipment].card_types.clone();
    assert!(
        equipment_types.core_types.contains(&CoreType::Artifact)
            && !equipment_types.core_types.contains(&CoreType::Creature),
        "anti-vacuity: the announced attachment is a noncreature artifact, so the host's \
         \"target creature\" filter cannot admit it: {equipment_types:?}"
    );

    let restored_state = admitted(persisted_json(runner.state())).map_err(|error| {
        format!(
            "a Magnetic Theft park whose facts satisfy their own slots' filters must be \
             admitted: {error:?}"
        )
    });
    let restored_state =
        restored_state.expect("reach: the honest two-operand announcement is admitted");

    // The resumed save pays the locked {R} and attaches from the restored state, not
    // from the pre-save runner.
    let mut resumed = GameRunner::from_state(restored_state);
    assert!(
        matches!(resumed.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the restored state still owes the manual mana payment, got {:?}",
        resumed.state().waiting_for
    );
    tap_land_for_mana(&mut resumed, mountain);
    resumed
        .act(GameAction::PassPriority)
        .expect("the restored save pays its locked cost");
    assert!(
        resumed.state().stack.iter().any(|entry| entry.id == spell),
        "reach guard: the resumed save put Magnetic Theft on the stack"
    );
    resumed.advance_until_stack_empty();
    assert!(
        resumed.state().stack.is_empty(),
        "reach guard: the resumed Magnetic Theft resolved"
    );
    // CR 701.3a + CR 301.5: the Aura/Equipment becomes attached to the creature it was
    // announced against, and control of the Equipment does not change.
    assert_eq!(
        resumed.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(creature)),
        "the resumed save attaches the announced Equipment to the announced creature"
    );
}

/// The Bone Splinters park: its announced target has been SACRIFICED to pay part
/// of the spell's mana, so the announcement pin is stale at the mana-payment park.
struct BoneSplintersPark {
    runner: GameRunner,
    spell: ObjectId,
    target: ObjectId,
    additional_cost_creature: ObjectId,
}

/// CR 601.2c + CR 601.2g-h: printed **Bone Splinters** is `{B}` for "As an
/// additional cost to cast this spell, sacrifice a creature. / Destroy target
/// creature." (Scryfall, oracle text verified; the checked-in fixture carries the
/// same oracle text and additional cost). The caster announces its OWN Grizzly
/// Bears as the target, names Savannah Lions for the additional sacrifice, and
/// then sacrifices the announced Bears to Ashnod's Altar for `{C}{C}` before the
/// mana payment, so the cast is parked with an already-moved target.
///
/// The additional-cost sacrifice itself cannot supply this shape: the engine pays
/// it at commit, after mana payment (`can_defer_spell_sacrifice_until_mana_payment`
/// defers every spell sacrifice to `pay_deferred_spell_sacrifices_at_commit`), so
/// answering that prompt leaves the permanent on the battlefield. A sacrifice to a
/// mana ability inside the payment window is the reachable way a legal Bone
/// Splinters cast loses its own target before the cast finishes, and the guard it
/// exercises — a stale pin beside a recorded creature requirement — is the same.
///
/// Reach guards pin the shape: the target was announced through the public
/// prompt, the additional cost was named through its own prompt, the sacrifice
/// really moved the announced Bears to the graveyard, the announcement pin is
/// stale, and the latch still records the creature the slot's filter admitted.
fn bone_splinters_parked_after_sacrificing_its_target() -> BoneSplintersPark {
    let db = shared_card_db().expect("the checked-in card fixture contains these cards");
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let spell = sc.add_real_card(P0, "Bone Splinters", Zone::Hand, db);
    let target = sc.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let additional_cost_creature = sc.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    let altar = sc.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    let swamp = sc.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    // The second legal target keeps the engine from auto-selecting the only one,
    // so the announcement goes through the public prompt.
    sc.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = sc.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Bone Splinters is announced");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: the cast asks for its target, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("P0's own Bears is a legal target for Bone Splinters");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "reach guard: the additional sacrifice cost is asked for, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![additional_cost_creature],
        })
        .expect("Savannah Lions pays the additional sacrifice cost");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the cast parks on the manual mana payment, got {:?}",
        runner.state().waiting_for
    );

    // CR 601.2g: mana abilities may be activated inside the payment window, and
    // the Altar's own cost sacrifices a creature.
    let altar_index = runner.state().objects[&altar]
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("Ashnod's Altar has its mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: altar_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Altar's mana ability begins");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "reach guard: the Altar asks for the creature it sacrifices, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![target],
        })
        .expect("the announced target pays the Altar's cost");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the spell's mana is still owed, so the cast is still parked on the payment \
         window; got {:?}",
        runner.state().waiting_for
    );
    tap_land_for_mana(&mut runner, swamp);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the black mana is floating and the cast is still parked, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Graveyard,
        "reach guard: the announced target really was sacrificed"
    );
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
        "reach guard: the sacrificed target already moved, so its live zone is not comparable"
    );
    let latched = parked
        .committed_target_facts
        .as_deref()
        .expect("reach guard: the engine-written park carries its announced-target latch");
    let latched_fact = latched
        .iter()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == target))
        .unwrap_or_else(|| {
            panic!("reach guard: the latch names the announced Bears; got {latched:?}")
        });
    assert!(
        fact_core_types(latched_fact).contains(&CoreType::Creature),
        "reach guard: the latch recorded the announced creature: {:?}",
        fact_core_types(latched_fact)
    );

    BoneSplintersPark {
        runner,
        spell,
        target,
        additional_cost_creature,
    }
}

/// CR 601.2c + CR 601.2i + CR 400.7: the honest save. Bone Splinters announced
/// P0's own creature and that creature was sacrificed to pay part of the spell's
/// cost, so the target's announcement pin is stale at the mana-payment park. A
/// stale pin makes the LIVE zone comparison invalid — it does not make the
/// RECORDED announcement optional — so the save must still be admitted, and the
/// resumed cast must publish the announced creature as its CR 601.2i history.
#[test]
fn a_bone_splinters_park_after_sacrificing_its_target_restores() {
    let BoneSplintersPark {
        runner,
        spell,
        target,
        additional_cost_creature,
    } = bone_splinters_parked_after_sacrificing_its_target();

    let restored = admitted(persisted_json(runner.state())).map_err(|error| {
        format!("an honest stale-pin park carrying its announcement must be admitted: {error:?}")
    });
    let restored_state = restored.expect("reach: the honest stale-pin park is admitted");

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
    assert_eq!(
        resumed.state().objects[&additional_cost_creature].zone,
        Zone::Graveyard,
        "reach guard: the resumed save paid the additional sacrifice cost"
    );

    let fact = declared_object_fact(resumed.state(), spell);
    assert_eq!(
        fact_object_id(&fact),
        target,
        "the resumed save's cast history names the announced target"
    );
    assert!(
        fact_core_types(&fact).contains(&CoreType::Creature),
        "the resumed save's cast history keeps the announced creature: {:?}",
        fact_core_types(&fact)
    );
}

/// CR 601.2c + CR 115.4: the same pack with the RECORDED type rewritten. Target
/// id, stated zone, token bit and every other field are the honest ones; only the
/// announcement snapshot's `Creature` is dropped. "Destroy target creature" never
/// admitted a non-creature, so this is a recorded contradiction — and the stale
/// pin (the target really did move) must not turn the recorded requirement into an
/// optional check, or the forged snapshot publishes as CR 601.2i cast history.
#[test]
fn a_bone_splinters_park_after_sacrificing_its_target_refuses_a_restated_type() {
    let BoneSplintersPark {
        runner,
        spell,
        target,
        ..
    } = bone_splinters_parked_after_sacrificing_its_target();

    // The control: this exact pack, unmodified, is admitted.
    admitted(persisted_json(runner.state())).expect("reach-guard: the honest pack is admitted");

    let mut state = runner.state().clone();
    let parked = state
        .pending_cast
        .as_deref_mut()
        .expect("reach-guard: the ManaPayment prompt owns the cast");
    let latched = parked
        .committed_target_facts
        .as_deref_mut()
        .expect("reach-guard: the engine-written park carries its announced-target latch");
    let fact = latched
        .iter_mut()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { id, .. } if *id == target))
        .expect("reach-guard: the latch names the announced Bears");
    let ActivationTargetFact::Object { id, zone, lki, .. } = fact else {
        unreachable!("filtered to the object fact above");
    };
    let refused_target = *id;
    assert_eq!(
        *zone,
        AnnouncedTargetZone::Known(Zone::Battlefield),
        "reach-guard: the honest fact states the battlefield it was announced on"
    );
    assert!(
        lki.card_types.contains(&CoreType::Creature),
        "reach-guard: the honest snapshot records the creature the slot's filter required"
    );
    // CR 613.1d: a recorded type line "target creature" never admitted, restated
    // for the same object in the same recorded zone.
    lki.card_types.retain(|kind| *kind != CoreType::Creature);

    match PersistedGameState::capture(state).into_game_state() {
        Err(PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason:
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter {
                    target: TargetRef::Object(refused),
                },
        }) => {
            assert_eq!(
                spell_object_id, spell,
                "the refusal names the cast it is about"
            );
            assert_eq!(
                refused, refused_target,
                "and the target whose recorded type was rewritten"
            );
        }
        other => panic!(
            "CR 601.2c + CR 115.4: a stale pin does not make the recorded creature requirement \
             optional; a snapshot restating the moved target's type must be refused, got {other:?}"
        ),
    }
}
