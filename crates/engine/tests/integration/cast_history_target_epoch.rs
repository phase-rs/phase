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
//!
//! **Devour in Flames** is the ORDINARY return-to-hand arm of the same class:
//! `{2}{R}` for "As an additional cost to cast this spell, return a land you
//! control to its owner's hand. / Devour in Flames deals 5 damage to target
//! creature or planeswalker." Returning the very land it targets is legal and is
//! the cheapest way to pay, and the pair below disagrees on the type axis for the
//! same reason the sacrifice pair does — but through the hand, not the graveyard:
//!
//! * **Dryad Arbor**'s printed type line is "Land Creature — Forest Dryad", so
//!   after being returned it is still a Creature in hand and a post-payment
//!   re-read happens to agree.
//! * **Mishra's Factory** returned the same way reverts to its printed "Land" and
//!   is no longer a creature, so a re-read records a non-creature for a cast
//!   whose announced target was a 2/2 artifact creature.
//!
//! The return-to-hand path moves the target *before* the payment frame resumes,
//! so it is the path on which an unlatched capture cannot be correct by accident
//! for both witnesses.
//!
//! CR 601.2c is satisfied either way: the caster announces the target through the
//! public `SelectTargets` prompt when several objects are legal, and the engine
//! announces the single legal one itself when there is only one. Both are the
//! caster's CR 601.2c announcement, so both must reach the CR 601.2i write with
//! the same facts — the pairs below run each witness down the automatic branch as
//! well as the interactive one.

use engine::game::filter::{spell_record_matches_filter, FilterContext};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{FilterProp, TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    ActivationTargetFact, CastPaymentMode, CostResume, GameState, PersistedGameState,
    PersistedRestoreError, SpellCastRecord, WaitingFor,
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

const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const LEYLINE_OF_THE_VOID: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";

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
///
/// CR 601.2c + CR 601.2h: a cast parked on an interactive cost prompt already
/// carries the announced-target latch on the `PendingCast` inside the prompt,
/// because the boundary that settles targets runs before any cost is paid. This
/// asserts it at the prompt itself, which is what makes every witness below a
/// latch-BEFORE-the-move test: when the cost card is returned or sacrificed, the
/// facts are already fixed and the later move cannot be what produced them.
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
        if let WaitingFor::PayCost {
            resume: CostResume::Spell { spell },
            ..
        } = &wf
        {
            assert!(
                spell.committed_target_facts.is_some(),
                "reach guard: the announced-target latch is already set at the cost prompt, \
                 before the cost moves anything"
            );
        }
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

/// CR 601.2c + CR 400.7 + CR 616.1: a cast parked on a replacement choice raised by
/// its own additional sacrifice cost must still be admitted with its announced
/// targets, and a save that cannot prove them must be refused *before* it resumes
/// with a history record that claims the cast declared nothing.
///
/// The target here is a **distinct surviving** creature, not the sacrificed one: it
/// is still on the battlefield when the cast resumes, so the paused carrier — not a
/// later lookup — is what preserves it.
#[test]
fn a_persisted_first_cast_sacrifice_pause_keeps_its_announced_targets_or_is_refused() {
    let mut sc = board();
    // Both replacements must apply to P0's sacrificed creature for CR 616.1 to
    // raise an ordering choice: Rest in Peace affects any graveyard, and Leyline of
    // the Void only an opponent's — so the opponent controls both.
    sc.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    sc.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID);
    let survivor = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let fodder = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spell = bone_splinters(&mut sc);
    sc.with_mana_pool(P0, pool(&[ManaType::Black]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    // Drive to the replacement-order choice CR 616.1 raises for the sacrifice.
    let mut saw_choice = false;
    for _ in 0..32 {
        let wf = runner.state().waiting_for.clone();
        match wf {
            WaitingFor::ReplacementChoice { .. } => {
                saw_choice = true;
                break;
            }
            WaitingFor::TargetSelection { .. } => runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(survivor)],
                })
                .expect("target the surviving creature"),
            WaitingFor::PayCost { .. } => runner
                .act(GameAction::SelectCards {
                    cards: vec![fodder],
                })
                .expect("sacrifice the fodder"),
            WaitingFor::ManaPayment { .. } => runner
                .act(GameAction::PassPriority)
                .expect("pay the black mana"),
            other => panic!("unexpected announcement prompt {other:?}"),
        };
    }
    assert!(
        saw_choice,
        "reach guard: the two graveyard replacements raise a CR 616.1 choice while the cast is parked"
    );

    let persisted = PersistedGameState::capture(runner.state().clone());
    let json = serde_json::to_value(&persisted).expect("the park serializes");

    // (a) A park that carries its announced targets is admitted.
    // `into_game_state` is `prepare_for_restore(Immediate)` + finalize, so an `Ok`
    // here is the admission decision itself.
    let restored_state = serde_json::from_value::<PersistedGameState>(json.clone())
        .expect("the park decodes")
        .into_game_state()
        .map_err(|error| {
            format!("a parked cast carrying its announced targets must be admitted: {error:?}")
        })
        .expect("admitted");

    // (b) The same park written before the latch existed cannot prove what the
    // spell declared, so it is refused at the shared restore boundary rather than
    // resumed into a falsely known-empty history record.
    fn strip_pending_facts(value: &mut serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                let mut stripped = map.remove("committed_target_facts").is_some();
                for nested in map.values_mut() {
                    stripped |= strip_pending_facts(nested);
                }
                stripped
            }
            serde_json::Value::Array(items) => items
                .iter_mut()
                .fold(false, |acc, item| strip_pending_facts(item) | acc),
            _ => false,
        }
    }
    let mut legacy = json.clone();
    assert!(
        strip_pending_facts(&mut legacy),
        "reach: the fixture really carried a latch to strip"
    );
    let refused = serde_json::from_value::<PersistedGameState>(legacy)
        .expect("the legacy park decodes")
        .into_game_state();
    assert!(
        matches!(
            refused,
            Err(PersistedRestoreError::UnknownPendingTargetFacts { .. })
        ),
        "a park without announced targets is refused: {:?}",
        refused.err()
    );

    // (c) The ADMITTED SAVE — not the in-memory park it was taken from — resumes to
    // a correct history. This is the half that would catch an admission which lets a
    // carrier through and then writes a falsely known-empty record.

    let mut restored = GameRunner::from_state(restored_state);
    assert_eq!(
        restored.state().objects[&survivor].zone,
        Zone::Battlefield,
        "reach guard: the announced target is a distinct survivor, still on the battlefield"
    );
    restored
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("order the replacement on the restored save");
    for _ in 0..16 {
        if matches!(restored.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        restored.act(GameAction::PassPriority).expect("settle");
    }
    let fact = declared_object_fact(restored.state(), spell);
    let ActivationTargetFact::Object { id, .. } = &fact else {
        panic!("the record declares an object target: {fact:?}");
    };
    assert_eq!(
        *id, survivor,
        "the RESUMED SAVE's history names the announced target, not the sacrificed fodder"
    );
    assert!(
        fact_core_types(&fact).contains(&CoreType::Creature),
        "the resumed save keeps the selection-time creature: {:?}",
        fact_core_types(&fact)
    );
    assert!(
        record_answers_targets_a_creature(restored.state(), spell),
        "the resumed save's record answers `targets a creature`"
    );
}

const DEVOUR_IN_FLAMES: &str =
    "As an additional cost to cast this spell, return a land you control to its owner's hand.\nDevour in Flames deals 5 damage to target creature or planeswalker.";

/// Dryad Arbor's mana ability, which its printed reminder text states verbatim as
/// `"{T}: Add {G}."`. Its printed type line — "Land Creature — Forest Dryad" — is
/// half of what makes it a witness: it is a legal "land you control" to return and
/// a legal "target creature" at the same time.
const DRYAD_ARBOR: &str = "{T}: Add {G}.";

fn devour_in_flames(sc: &mut GameScenario) -> ObjectId {
    sc.add_spell_to_hand_from_oracle(P0, "Devour in Flames", false, DEVOUR_IN_FLAMES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id()
}

/// Give Dryad Arbor its printed Land half. `add_creature_from_oracle` supplies
/// the Creature half (type, power, toughness); the engine's other Dryad-Arbor
/// fixtures complete the printed type line the same way.
fn add_printed_land_type(runner: &mut GameRunner, land: ObjectId) {
    let arbor = runner
        .state_mut()
        .objects
        .get_mut(&land)
        .expect("reach: the Dryad Arbor fixture exists");
    arbor.card_types.core_types.push(CoreType::Land);
    arbor.base_card_types = arbor.card_types.clone();
}

/// CR 601.2c: which of the two announcement branches the witness takes. Both are
/// the caster's announcement; they differ only in who resolves the choice when
/// exactly one object is legal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TargetDeclaration {
    /// A second legal target (an opponent's Grizzly Bears) forces the public
    /// `SelectTargets` prompt, so the caster announces the target themselves and
    /// the boundary that latches it is `finish_pending_cast_cost_or_pay`.
    Interactive,
    /// The returned land is the only legal target, so the engine announces it
    /// without a prompt. This branch hands the cast straight to cost payment and
    /// never passes that boundary, so it must latch the announcement on the way.
    Automatic,
}

/// CR 601.2c + CR 601.2h: announce Devour in Flames with `land` as its target —
/// through the public `SelectTargets` prompt or by the engine's automatic
/// announcement, per `declaration` — then pay the additional cost by returning
/// that same land through the public `SelectCards` prompt. Mana is supplied up
/// front so the return is the only cost prompt. Returns the prompts the
/// announcement answered, in order.
fn cast_devour_returning_its_target(
    runner: &mut GameRunner,
    spell: ObjectId,
    land: ObjectId,
    declaration: TargetDeclaration,
) -> Vec<String> {
    cast(runner, spell, CastPaymentMode::Auto);
    let prompts = drive_to_priority(runner, land, land);
    match declaration {
        TargetDeclaration::Interactive => assert!(
            prompts.iter().any(|prompt| prompt == "TargetSelection"),
            "reach guard: the target was announced through the public `SelectTargets` prompt; \
             prompts {prompts:?}"
        ),
        TargetDeclaration::Automatic => assert!(
            !prompts.iter().any(|prompt| prompt == "TargetSelection"),
            "reach guard: the sole legal target was announced by the engine's automatic \
             branch (CR 601.2c), with no `SelectTargets` prompt; prompts {prompts:?}"
        ),
    }
    assert!(
        prompts.iter().any(|prompt| prompt == "PayCost"),
        "reach guard: the return-to-hand additional cost was paid through the public \
         `SelectCards` prompt; prompts {prompts:?}"
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Hand,
        "reach guard: the additional cost returned the targeted land to its owner's hand \
         before the cast finished; prompts {prompts:?}"
    );
    assert!(
        recorded_cast(runner.state(), spell)
            .declared_targets()
            .len()
            == 1,
        "reach guard: the committed cast declared exactly one target"
    );
    prompts
}

/// What the cast history says about one returned-land witness.
struct RecordedReturnedTarget {
    /// The recorded target's type line, as the record latched it.
    types: Vec<CoreType>,
    /// `FilterProp::Targets { Typed(Creature) }` — the shape Head of the Class's
    /// first-spell gate carries over the spell history.
    answers_targets_a_creature: bool,
}

/// Run one Devour in Flames witness end to end and read back what the cast
/// history recorded for it.
fn devour_witness(kind: ReturnedLand, declaration: TargetDeclaration) -> RecordedReturnedTarget {
    let mut sc = board();
    let land = match kind {
        ReturnedLand::DryadArbor => sc
            .add_creature_from_oracle(P0, "Dryad Arbor", 1, 1, DRYAD_ARBOR)
            .id(),
        ReturnedLand::AnimatedFactory => sc
            .add_land_from_oracle(P0, "Mishra's Factory", MISHRA_S_FACTORY)
            .id(),
    };
    let spell = devour_in_flames(&mut sc);
    // A second legal target (an opponent's creature) keeps the engine from
    // auto-selecting the only one, so the caster really announces the land
    // through the public `SelectTargets` prompt. The automatic row omits it: the
    // returned land is then the only legal target and the engine announces it.
    if declaration == TargetDeclaration::Interactive {
        sc.add_creature(P1, "Grizzly Bears", 2, 2);
    }
    // CR 601.2h: mana is supplied separately so the additional cost is the only
    // thing the cast has to ask for. An animated Factory also owes the `{1}`
    // animation below, hence the fourth unit.
    sc.with_mana_pool(
        P0,
        pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Red,
        ]),
    );
    let mut runner = sc.build();

    match kind {
        ReturnedLand::DryadArbor => add_printed_land_type(&mut runner, land),
        ReturnedLand::AnimatedFactory => {
            // CR 613.1d: the printed `{1}` animation's AddType Creature applies in
            // layer 4, so it becomes a 2/2 artifact creature while its own "It's
            // still a land." keeps the Land type.
            runner
                .act(GameAction::ActivateAbility {
                    source_id: land,
                    ability_index: 1,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("reach: Mishra's Factory activates its animation");
            settle_stack(&mut runner);
        }
    }
    assert!(
        runner.state().objects[&land]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach guard ({}): the announced target is a creature",
        kind.label()
    );

    cast_devour_returning_its_target(&mut runner, spell, land, declaration);

    // Anti-vacuity: once the cost has returned it, the card's own type line no
    // longer says creature for the animated Factory, so the recorded fact can
    // only come from the announcement-time latch.
    if kind == ReturnedLand::AnimatedFactory {
        assert!(
            !runner.state().objects[&land]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "anti-vacuity: the returned card is no longer a creature, so a \
             post-payment raw-id capture would record Land"
        );
    }

    let fact = declared_object_fact(runner.state(), spell);
    RecordedReturnedTarget {
        types: fact_core_types(&fact),
        answers_targets_a_creature: record_answers_targets_a_creature(runner.state(), spell),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReturnedLand {
    DryadArbor,
    AnimatedFactory,
}

impl ReturnedLand {
    fn label(self) -> &'static str {
        match self {
            ReturnedLand::DryadArbor => "Dryad Arbor",
            ReturnedLand::AnimatedFactory => "animated Mishra's Factory",
        }
    }
}

/// Printed Dryad Arbor reads "Land Creature — Forest Dryad", so returning it
/// leaves it a creature in hand: the witness a coincidentally-correct re-read
/// cannot tell apart from a latched one.
#[test]
fn devour_in_flames_records_a_returned_dryad_arbor_as_the_creature_it_was() {
    let recorded = devour_witness(ReturnedLand::DryadArbor, TargetDeclaration::Interactive);
    assert!(
        recorded.types.contains(&CoreType::Creature),
        "Dryad Arbor: the record keeps the announced creature: {:?}",
        recorded.types
    );
    assert!(
        recorded.answers_targets_a_creature,
        "Dryad Arbor: the record answers `targets a creature`"
    );
}

/// An animated Mishra's Factory returned as Devour in Flames' own additional
/// cost reverts to its printed "Land" type line the moment it leaves the
/// battlefield. The record must keep the creature the caster announced.
#[test]
fn devour_in_flames_records_a_returned_animated_factory_as_the_creature_it_was() {
    let recorded = devour_witness(
        ReturnedLand::AnimatedFactory,
        TargetDeclaration::Interactive,
    );
    assert!(
        recorded.types.contains(&CoreType::Creature),
        "animated Mishra's Factory: the record keeps the selection-time creature the \
         caster announced, not the returned card's printed type line: {:?}",
        recorded.types
    );
    assert!(
        recorded.answers_targets_a_creature,
        "animated Mishra's Factory: the record answers `targets a creature`, which is \
         what Head of the Class's first-spell gate counts"
    );
}

/// CR 115.1 + CR 115.9: the two returned creature lands are the same declared
/// fact — "this cast targeted a creature" — so the history query must answer them
/// alike. A capture taken after the return splits them: Dryad Arbor keeps its
/// printed creature type in hand, the Factory does not.
#[test]
fn devour_in_flames_answers_the_targets_a_creature_query_alike_for_both_returned_lands() {
    let dryad = devour_witness(ReturnedLand::DryadArbor, TargetDeclaration::Interactive);
    let factory = devour_witness(
        ReturnedLand::AnimatedFactory,
        TargetDeclaration::Interactive,
    );
    assert_eq!(
        dryad.answers_targets_a_creature,
        factory.answers_targets_a_creature,
        "the history query answers the two declared creature targets alike: \
         Dryad Arbor {:?} (types {:?}) vs animated Mishra's Factory {:?} (types {:?})",
        dryad.answers_targets_a_creature,
        dryad.types,
        factory.answers_targets_a_creature,
        factory.types
    );
    assert!(
        dryad.answers_targets_a_creature,
        "both returned creature lands answer `targets a creature`"
    );
}

/// CR 601.2c + CR 601.2h: the automatic-announcement sibling of the animated
/// Factory row. With no second legal target the engine announces the sole one
/// itself and hands the cast straight to cost payment, so the target facts have
/// to be latched on that branch — otherwise the return-to-hand cost moves the
/// land and the CR 601.2i write records the printed Land it became in hand.
#[test]
fn devour_in_flames_records_an_automatically_announced_animated_factory_as_the_creature_it_was() {
    let recorded = devour_witness(ReturnedLand::AnimatedFactory, TargetDeclaration::Automatic);
    assert!(
        recorded.types.contains(&CoreType::Creature),
        "animated Mishra's Factory (automatic announcement): the record keeps the \
         selection-time creature the caster announced, not the returned card's printed \
         type line: {:?}",
        recorded.types
    );
    assert!(
        recorded.answers_targets_a_creature,
        "animated Mishra's Factory (automatic announcement): the record answers \
         `targets a creature`, which is what Head of the Class's first-spell gate counts"
    );
}

/// CR 115.1 + CR 115.9: a sole legal Dryad Arbor announced by the engine is the
/// same declared fact as one announced through the prompt, so the automatic
/// branch must answer the history query the same way.
#[test]
fn devour_in_flames_records_an_automatically_announced_dryad_arbor_as_the_creature_it_was() {
    let recorded = devour_witness(ReturnedLand::DryadArbor, TargetDeclaration::Automatic);
    assert!(
        recorded.types.contains(&CoreType::Creature),
        "Dryad Arbor (automatic announcement): the record keeps the announced creature: {:?}",
        recorded.types
    );
    assert!(
        recorded.answers_targets_a_creature,
        "Dryad Arbor (automatic announcement): the record answers `targets a creature`"
    );
}

/// CR 115.1 + CR 115.9: the two announcement branches are one CR 601.2c fact, so
/// the same witness must record the same answer whichever branch announced it.
#[test]
fn devour_in_flames_answers_the_targets_a_creature_query_alike_across_both_announcement_branches() {
    let interactive = devour_witness(
        ReturnedLand::AnimatedFactory,
        TargetDeclaration::Interactive,
    );
    let automatic = devour_witness(ReturnedLand::AnimatedFactory, TargetDeclaration::Automatic);
    assert_eq!(
        interactive.answers_targets_a_creature,
        automatic.answers_targets_a_creature,
        "the history query answers the two announcement branches alike: interactive {:?} \
         (types {:?}) vs automatic {:?} (types {:?})",
        interactive.answers_targets_a_creature,
        interactive.types,
        automatic.answers_targets_a_creature,
        automatic.types
    );
    assert!(
        automatic.answers_targets_a_creature,
        "the automatically announced animated Factory answers `targets a creature`"
    );
}
