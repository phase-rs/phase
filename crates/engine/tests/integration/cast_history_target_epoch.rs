//! CR 115.1 + CR 115.9 + CR 601.2c + CR 601.2i + CR 400.7: the declared-target
//! facts a spell-cast record carries must describe the targets as the caster
//! announced them, not whatever object later occupies their storage id.
//!
//! Printed **Bone Splinters** is `{B}` for "As an additional cost to cast this
//! spell, sacrifice a creature. / Destroy target creature." — it can target and
//! sacrifice the same permanent. Both witnesses below do exactly that, and the
//! two of them disagree about the type axis unless the facts are latched at
//! target commitment (CR 601.2c), before the cost is paid (CR 601.2h):
//!
//! * **Grizzly Bears**, a printed 2/2 Bear, is a Creature on the battlefield and
//!   a Creature in the graveyard, so a post-payment re-read happens to agree.
//! * **Mishra's Factory** is a Land that its own `{1}` ability turns into a
//!   "2/2 Assembly-Worker artifact creature … still a land". Recaptured by raw id
//!   after the sacrifice, the same storage id is the graveyard card, and a zone
//!   departure restores the printed baseline type line — Land. Selection-time
//!   facts record the announced creature.
//!
//! CR 115.9 gives the selection-time reading for a history predicate and CR
//! 115.9b ignores a departed target for a current-state read; neither authorizes
//! substituting the moved object's new-incarnation characteristics (CR 400.7),
//! so the record must keep what was announced. Head of the Class — "The first
//! spell you cast during each of your turns that targets a creature costs {W}{B}
//! less to cast" — is the consumer that reasons over exactly this type axis.
//!
//! Both payment modes are exercised for each witness: automatic payment
//! sacrifices inside the paying frame, and manual payment parks the cast on the
//! `ManaPayment` prompt and resumes it, which is the re-entry path that used to
//! recapture after the sacrifice.

use engine::game::filter::{spell_record_matches_filter, FilterContext};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{FilterProp, TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    ActivationTargetFact, CastPaymentMode, GameState, SpellCastRecord, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BONE_SPLINTERS: &str =
    "As an additional cost to cast this spell, sacrifice a creature.\nDestroy target creature.";

/// Printed Mishra's Factory, verbatim: the `{1}` activation is the animation
/// that makes it a creature while it remains a land.
const MISHRA_S_FACTORY: &str = "{T}: Add {C}.\n{1}: This land becomes a 2/2 Assembly-Worker artifact creature until end of turn. It's still a land.\n{T}: Target Assembly-Worker creature gets +1/+1 until end of turn.";

fn pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|t| ManaUnit::new(*t, ObjectId(0), false, vec![]))
        .collect()
}

fn board() -> GameScenario {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc
}

fn bone_splinters(sc: &mut GameScenario) -> ObjectId {
    sc.add_spell_to_hand_from_oracle(P0, "Bone Splinters", false, BONE_SPLINTERS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id()
}

fn cast(runner: &mut GameRunner, spell: ObjectId, mode: CastPaymentMode) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: mode,
        })
        .expect("the cast is announced");
}

/// Answers each announcement prompt until priority opens, returning the prompts
/// it answered — the reach guard that the payment really paused.
fn drive_to_priority(
    runner: &mut GameRunner,
    target: ObjectId,
    sacrifice: ObjectId,
) -> Vec<String> {
    let mut prompts = Vec::new();
    for _ in 0..32 {
        let wf = runner.state().waiting_for.clone();
        let name = format!("{wf:?}")
            .split([' ', '{', '('])
            .next()
            .unwrap_or_default()
            .to_string();
        let action = match wf {
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => return prompts,
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            },
            WaitingFor::PayCost { .. } => GameAction::SelectCards {
                cards: vec![sacrifice],
            },
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

/// The recorded cast of `spell`, found by the stable id link.
fn recorded_cast(state: &GameState, spell: ObjectId) -> &SpellCastRecord {
    state
        .spells_cast_this_turn_by_player
        .values()
        .flat_map(|records| records.iter())
        .find(|record| record.spell_object_id == Some(spell))
        .expect("reach: the committed cast is recorded in the turn history")
}

/// `FilterProp::Targets { Typed(Creature) }` — the shape Head of the Class's
/// gate carries over the spell history.
fn targets_a_creature_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Targets {
        filter: Box::new(TargetFilter::Typed(TypedFilter::creature())),
    }]))
}

/// The one latched object target of `spell`'s record.
fn declared_object_fact(state: &GameState, spell: ObjectId) -> ActivationTargetFact {
    let record = recorded_cast(state, spell);
    record
        .declared_targets()
        .iter()
        .find(|fact| matches!(fact, ActivationTargetFact::Object { .. }))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "reach: the record declares the announced object target; got {:?}",
                record.declared_targets()
            )
        })
}

fn fact_core_types(fact: &ActivationTargetFact) -> Vec<CoreType> {
    match fact {
        ActivationTargetFact::Object { lki, .. } => lki.card_types.clone(),
        ActivationTargetFact::Player(_) => Vec::new(),
    }
}

fn record_answers_targets_a_creature(state: &GameState, spell: ObjectId) -> bool {
    let record = recorded_cast(state, spell);
    let ctx = FilterContext::neutral();
    spell_record_matches_filter(record, &targets_a_creature_filter(), P0, state, &ctx)
}

/// Ordinary creature: Bone Splinters targets and sacrifices the same Grizzly
/// Bears under both payment modes, and the record keeps it a Creature.
#[test]
fn bone_splinters_records_an_ordinary_sacrificed_target_as_a_creature() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        let mut sc = board();
        let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let spell = bone_splinters(&mut sc);
        sc.with_mana_pool(P0, pool(&[ManaType::Black]));
        let mut runner = sc.build();

        cast(&mut runner, spell, mode);
        let prompts = drive_to_priority(&mut runner, bears, bears);

        assert_eq!(
            runner.state().objects[&bears].zone,
            Zone::Graveyard,
            "reach guard ({mode:?}): the target was sacrificed as the additional cost"
        );
        let fact = declared_object_fact(runner.state(), spell);
        assert!(
            fact_core_types(&fact).contains(&CoreType::Creature),
            "{mode:?}: the record keeps the announced creature: {:?}",
            fact_core_types(&fact)
        );
        assert!(
            record_answers_targets_a_creature(runner.state(), spell),
            "{mode:?}: the record answers `targets a creature`; prompts {prompts:?}"
        );
    }
}

/// Animated land: Bone Splinters targets and sacrifices the same Mishra's
/// Factory while it is a 2/2 artifact creature. The record must keep the
/// selection-time creature, not the graveyard card's printed Land type line.
#[test]
fn bone_splinters_records_an_animated_sacrificed_target_as_the_creature_it_was() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        let mut sc = board();
        let factory = sc
            .add_land_from_oracle(P0, "Mishra's Factory", MISHRA_S_FACTORY)
            .id();
        let spell = bone_splinters(&mut sc);
        sc.with_mana_pool(P0, pool(&[ManaType::Colorless, ManaType::Black]));
        let mut runner = sc.build();

        // CR 613.1d + CR 305.7: activate the printed `{1}` animation. The land
        // becomes an artifact creature and stays a land.
        runner
            .act(GameAction::ActivateAbility {
                source_id: factory,
                ability_index: 1,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach: Mishra's Factory activates its animation");
        settle_stack(&mut runner);
        assert!(
            runner.state().objects[&factory]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "reach guard ({mode:?}): the animation made the Factory a creature"
        );

        cast(&mut runner, spell, mode);
        let prompts = drive_to_priority(&mut runner, factory, factory);

        assert_eq!(
            runner.state().objects[&factory].zone,
            Zone::Graveyard,
            "reach guard ({mode:?}): the animated target was sacrificed as the cost"
        );
        let fact = declared_object_fact(runner.state(), spell);
        let types = fact_core_types(&fact);
        assert!(
            !runner.state().objects[&factory]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "anti-vacuity ({mode:?}): the departed card is no longer a creature, so a \
             post-payment raw-id capture would record Land — the fact below can only \
             come from the announcement-time latch"
        );
        assert!(
            types.contains(&CoreType::Creature),
            "{mode:?}: the record keeps the selection-time creature the caster \
             announced, not the departed card's printed type line: {types:?}"
        );
        assert!(
            record_answers_targets_a_creature(runner.state(), spell),
            "{mode:?}: the record answers `targets a creature`, which is what \
             Head of the Class's first-spell gate counts; prompts {prompts:?}"
        );
    }
}
