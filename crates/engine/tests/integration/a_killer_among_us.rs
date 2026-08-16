//! S07 — A Killer Among Us (enchantment, {4}{G}).
//!
//! Oracle:
//!   When this enchantment enters, create a 1/1 white Human creature token,
//!   a 1/1 blue Merfolk creature token, and a 1/1 red Goblin creature token.
//!   Then secretly choose Human, Merfolk, or Goblin.
//!   Sacrifice this enchantment, Reveal the creature type you chose: If target
//!   attacking creature token is the chosen type, put three +1/+1 counters on
//!   it and it gains deathtouch until end of turn.
//!
//! §5 cast-level discriminating tests — plan
//! `.planning/coverage-analysis/S07-A-KILLER-PLAN.md`. Every runtime test drives
//! the real `apply()` pipeline (GameScenario + GameRunner) and asserts measured
//! state deltas (token count/colors/PT, +1/+1 counters, Deathtouch grant,
//! legal-target set), never AST-internal flags.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityCost, ChoiceType, Effect, FilterProp, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const P1: PlayerId = PlayerId(1);

const ORACLE: &str = "When this enchantment enters, create a 1/1 white Human creature token, a 1/1 blue Merfolk creature token, and a 1/1 red Goblin creature token. Then secretly choose Human, Merfolk, or Goblin.\nSacrifice this enchantment, Reveal the creature type you chose: If target attacking creature token is the chosen type, put three +1/+1 counters on it and it gains deathtouch until end of turn.";

// --- helpers ---------------------------------------------------------------

/// Collect every `Effect` in an ability's `sub_ability` chain (root first).
fn chain_effects(def: &engine::types::ability::AbilityDefinition) -> Vec<&Effect> {
    let mut out = Vec::new();
    let mut cur = Some(def);
    while let Some(d) = cur {
        out.push(&*d.effect);
        cur = d.sub_ability.as_deref();
    }
    out
}

fn parse_killer() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        ORACLE,
        "A Killer Among Us",
        &[],
        &["Enchantment".to_string()],
        &[],
    )
}

/// Build a scenario at PreCombatMain (P0's turn), put A Killer in P0's hand as a
/// pure enchantment, ample green mana to cover {4}{G}, and return the runner +
/// the enchantment's ObjectId (stable across zone changes).
fn setup() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Ample mana so any attached cost is auto-payable.
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let killer = {
        let mut b = scenario.add_creature_to_hand(P0, "A Killer Among Us", 0, 0);
        b.from_oracle_text(ORACLE).as_enchantment();
        b.id()
    };
    // Populate a broad creature-type catalog so a candidate-restriction REGRESSION
    // (options -> full catalog) is visibly different from the restricted 3-list.
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec![
        "Human".into(),
        "Merfolk".into(),
        "Goblin".into(),
        "Dragon".into(),
        "Elf".into(),
        "Zombie".into(),
    ];
    (runner, killer)
}

/// Cast A Killer, resolve the ETB (creates 3 tokens), and stop at the pending
/// `NamedChoice`. Returns after casting; caller answers the choice.
fn cast_to_named_choice(runner: &mut GameRunner, killer: ObjectId) {
    let card_id = runner.state().objects[&killer].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: killer,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast A Killer Among Us");
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::NamedChoice { .. } => return,
            WaitingFor::OrderTriggers { .. } => {
                runner.advance_until_stack_empty();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    panic!("stack emptied without presenting the secretly-choose NamedChoice");
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    panic!("could not pass priority toward the ETB choice");
                }
            }
            other => panic!("unexpected window before NamedChoice: {other:?}"),
        }
    }
    panic!("never reached the secretly-choose NamedChoice");
}

/// Find the (unique) battlefield token owned by P0 whose name matches.
fn find_token(runner: &GameRunner, name: &str) -> ObjectId {
    let ids: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.name == name)
        })
        .collect();
    assert_eq!(
        ids.len(),
        1,
        "expected exactly one {name} token, found {ids:?}"
    );
    ids[0]
}

fn clear_sickness(runner: &mut GameRunner, id: ObjectId) {
    let turn = runner.state().turn_number;
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.summoning_sick = false;
    obj.entered_battlefield_turn = Some(turn.saturating_sub(1));
}

fn counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn has_deathtouch(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Deathtouch)
}

/// Drive: cast + choose `chosen`, clear sickness on `attacker_name`, advance to
/// combat, declare that token attacking P1, then activate the sacrifice ability
/// (index 0) targeting it. Returns (runner, killer, attacker_token).
fn drive_sacrifice_against(chosen: &str, attacker_name: &str) -> (GameRunner, ObjectId, ObjectId) {
    let (mut runner, killer) = setup();
    cast_to_named_choice(&mut runner, killer);
    runner
        .act(GameAction::ChooseOption {
            choice: chosen.to_string(),
        })
        .expect("answer the secretly-choose");

    let attacker = find_token(&runner, attacker_name);
    clear_sickness(&mut runner, attacker);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare the token as an attacker");

    runner.activate(killer, 0).target_object(attacker).resolve();
    (runner, killer, attacker)
}

// --- §5 tests --------------------------------------------------------------

/// #1 — ETB creates exactly the three typed 1/1 tokens and records the chosen
/// type. Discriminates: reverting the enumeration parse swallows the choose
/// clause, so no chosen attribute is recorded (`chosen_creature_type()` None).
#[test]
fn etb_creates_three_typed_tokens_and_records_chosen_type() {
    let (mut runner, killer) = setup();
    cast_to_named_choice(&mut runner, killer);

    // Exactly three P0 tokens, one per printed color/type, all 1/1.
    let tokens: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects.get(id).is_some_and(|o| o.is_token))
        .collect();
    assert_eq!(tokens.len(), 3, "ETB must create exactly 3 tokens");

    for (name, color) in [
        ("Human", engine::types::mana::ManaColor::White),
        ("Merfolk", engine::types::mana::ManaColor::Blue),
        ("Goblin", engine::types::mana::ManaColor::Red),
    ] {
        let id = find_token(&runner, name);
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.power, Some(1), "{name} token power");
        assert_eq!(obj.toughness, Some(1), "{name} token toughness");
        assert!(
            obj.color.contains(&color),
            "{name} token must be {color:?}, colors = {:?}",
            obj.color
        );
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == name),
            "{name} token must have subtype {name}, got {:?}",
            obj.card_types.subtypes
        );
    }

    runner
        .act(GameAction::ChooseOption {
            choice: "Goblin".to_string(),
        })
        .expect("choose Goblin");

    assert_eq!(
        runner.state().objects[&killer].chosen_creature_type(),
        Some("Goblin"),
        "the enchantment must record the secretly-chosen creature type"
    );
}

/// #2 — candidate restriction (design Q1): the pending choice offers exactly the
/// printed three, in source order. Discriminates: reverting N1/compute_options
/// yields the full `all_creature_types` catalog (would contain "Dragon").
#[test]
fn named_choice_offers_only_the_printed_three_in_order() {
    let (mut runner, killer) = setup();
    cast_to_named_choice(&mut runner, killer);

    let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
        panic!("expected a NamedChoice window");
    };
    assert_eq!(
        options,
        vec![
            "Human".to_string(),
            "Merfolk".to_string(),
            "Goblin".to_string()
        ],
        "candidate set must be the printed enumeration, order-preserved — not the full catalog"
    );
}

/// #3 — matching attacking token → three +1/+1 counters + Deathtouch, AND proves
/// the post-sacrifice LKI read (design Q3 crux). When the ability resolves the
/// enchantment is already in the graveyard (sacrificed as a cost), so the source
/// chosen-type read must survive that zone change. Discriminates: revert the gate
/// and this passes vacuously; break the LKI read and the gate goes false → 0
/// counters, no Deathtouch.
#[test]
fn matching_attacker_gains_three_counters_and_deathtouch_post_sacrifice() {
    let (runner, killer, goblin) = drive_sacrifice_against("Goblin", "Goblin");

    // The enchantment was sacrificed as a cost — it must be gone from play.
    assert_ne!(
        runner.state().objects[&killer].zone,
        engine::types::zones::Zone::Battlefield,
        "the enchantment must have been sacrificed as an activation cost"
    );

    assert_eq!(
        counters(&runner, goblin),
        3,
        "matching chosen-type attacker must gain three +1/+1 counters (LKI read post-sacrifice)"
    );
    let obj = &runner.state().objects[&goblin];
    assert_eq!(
        obj.power,
        Some(4),
        "Goblin token becomes 4/4 with 3 counters"
    );
    assert_eq!(obj.toughness, Some(4), "Goblin token becomes 4/4");
    assert!(
        has_deathtouch(&runner, goblin),
        "matching attacker must gain Deathtouch until end of turn"
    );
}

/// #4 — non-matching attacking token → nothing. Chosen = Goblin but the Human
/// token attacks. Discriminates the `IsChosenCreatureType` gate: a vacuous
/// "always apply" parse would still add counters here.
#[test]
fn non_matching_attacker_gains_nothing() {
    let (runner, _killer, human) = drive_sacrifice_against("Goblin", "Human");
    assert_eq!(
        counters(&runner, human),
        0,
        "non-matching attacker must gain NO counters (gate discriminates)"
    );
    assert!(
        !has_deathtouch(&runner, human),
        "non-matching attacker must NOT gain Deathtouch"
    );
}

/// #5 — the ability's target must be an ATTACKING token. With no attackers
/// declared, none of the three tokens is a legal target. Discriminates the
/// `Attacking` leg of the target filter.
#[test]
fn target_requires_an_attacking_token() {
    let (mut runner, killer) = setup();
    cast_to_named_choice(&mut runner, killer);
    runner
        .act(GameAction::ChooseOption {
            choice: "Goblin".to_string(),
        })
        .expect("choose Goblin");

    // No attackers declared. Announce the sacrifice ability and inspect the
    // target slot: the non-attacking Goblin token must not be a legal target.
    let goblin = find_token(&runner, "Goblin");
    let announce = runner.act(GameAction::ActivateAbility {
        source_id: killer,
        ability_index: 0,
    });

    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            let legal: Vec<ObjectId> = target_slots
                .iter()
                .flat_map(|s| s.legal_targets.iter())
                .filter_map(|t| match t {
                    engine::types::ability::TargetRef::Object(o) => Some(*o),
                    _ => None,
                })
                .collect();
            assert!(
                !legal.contains(&goblin),
                "a non-attacking token must not be a legal target; legal = {legal:?}"
            );
            assert!(
                legal.is_empty(),
                "no creature is attacking, so there must be no legal target; legal = {legal:?}"
            );
        }
        // Alternatively the engine rejects the activation outright for want of a
        // legal target — also acceptable and proves the Attacking requirement.
        _ => {
            assert!(
                announce.is_err(),
                "with no legal (attacking) target the activation must be rejected, \
                 got waiting_for = {:?}",
                runner.state().waiting_for
            );
        }
    }
}

/// #6 — multi-authority provenance (hostile fixture): two A-Killer enchantments,
/// A chose Goblin, B chose Merfolk (B chosen LAST). Each is sacrificed against
/// its own Goblin attacker. Only A (Goblin-chooser) grants counters. Because B's
/// choice is the most recent, a GLOBAL last-named-choice read would make A's
/// Goblin fail — so `a_goblin == 3` proves the read is source-scoped LKI.
#[test]
fn multi_authority_read_is_source_scoped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..12)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let killer_a = {
        let mut b = scenario.add_creature_to_hand(P0, "Killer A", 0, 0);
        b.from_oracle_text(ORACLE).as_enchantment();
        b.id()
    };
    let killer_b = {
        let mut b = scenario.add_creature_to_hand(P0, "Killer B", 0, 0);
        b.from_oracle_text(ORACLE).as_enchantment();
        b.id()
    };
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Human".into(), "Merfolk".into(), "Goblin".into()];

    // A enters and chooses Goblin.
    cast_to_named_choice(&mut runner, killer_a);
    runner
        .act(GameAction::ChooseOption {
            choice: "Goblin".to_string(),
        })
        .expect("A chooses Goblin");
    let a_goblin = find_token(&runner, "Goblin");

    // B enters and chooses Merfolk (the LAST choice made).
    cast_to_named_choice(&mut runner, killer_b);
    runner
        .act(GameAction::ChooseOption {
            choice: "Merfolk".to_string(),
        })
        .expect("B chooses Merfolk");
    // A second Goblin token (B's) — pick the one that isn't a_goblin.
    let b_goblin = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            *id != a_goblin
                && runner
                    .state()
                    .objects
                    .get(id)
                    .is_some_and(|o| o.is_token && o.name == "Goblin")
        })
        .expect("B's Goblin token");

    clear_sickness(&mut runner, a_goblin);
    clear_sickness(&mut runner, b_goblin);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (a_goblin, AttackTarget::Player(P1)),
            (b_goblin, AttackTarget::Player(P1)),
        ])
        .expect("both Goblins attack");

    // Sacrifice A (Goblin-chooser) targeting A's Goblin → 3 counters.
    runner
        .activate(killer_a, 0)
        .target_object(a_goblin)
        .resolve();
    // Sacrifice B (Merfolk-chooser) targeting B's Goblin → 0.
    runner
        .activate(killer_b, 0)
        .target_object(b_goblin)
        .resolve();

    assert_eq!(
        counters(&runner, a_goblin),
        3,
        "A chose Goblin → its Goblin attacker gains 3 counters even though B chose Merfolk LAST \
         (source-scoped read, not global last-choice)"
    );
    assert_eq!(
        counters(&runner, b_goblin),
        0,
        "B chose Merfolk → its Goblin attacker gains nothing"
    );
}

/// #7 — parser SHAPE. Confirms the corrected AST the runtime tests depend on:
/// ETB chain = 3 Token + one Choose{CreatureType{options}, persist}; the
/// activated ability has a single Sacrifice(SelfRef) cost (no phantom second
/// Sacrifice, no Reveal), an IsChosenCreatureType gate bound to slot 0, and its
/// counters/deathtouch target the attacking token (not SelfRef).
#[test]
fn parse_shape_matches_corrected_ast() {
    let parsed = parse_killer();

    // --- ETB trigger: 3 Token + Choose ---
    assert_eq!(parsed.triggers.len(), 1, "one ETB trigger");
    let etb = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("ETB has an effect chain");
    let effects = chain_effects(etb);

    let token_names: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Token { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        token_names,
        vec!["Human", "Merfolk", "Goblin"],
        "ETB creates all three tokens in order (middle not dropped)"
    );

    let choose = effects
        .iter()
        .find_map(|e| match e {
            Effect::Choose {
                choice_type,
                persist,
                ..
            } => Some((choice_type, *persist)),
            _ => None,
        })
        .expect("ETB chain contains a Choose");
    assert!(choose.1, "choose must persist onto the enchantment");
    assert_eq!(
        *choose.0,
        ChoiceType::creature_type_from(vec![
            "Human".to_string(),
            "Merfolk".to_string(),
            "Goblin".to_string(),
        ]),
        "restricted creature-type option list, source order"
    );

    // --- activated sacrifice ability ---
    assert_eq!(parsed.abilities.len(), 1, "one activated ability");
    let ab = &parsed.abilities[0];

    // (b) single Sacrifice(SelfRef), no residual/second cost, no Reveal.
    match ab.cost.as_ref().expect("sacrifice cost present") {
        AbilityCost::Sacrifice(sc) => assert_eq!(
            sc.target,
            TargetFilter::SelfRef,
            "the sole cost sacrifices THIS enchantment"
        ),
        other => panic!("expected a single Sacrifice(SelfRef) cost, got {other:?}"),
    }

    // (c) IsChosenCreatureType gate bound to the declared slot 0.
    match ab.condition.as_ref().expect("gate present") {
        AbilityCondition::TargetMatchesFilter {
            filter,
            subject_slot,
            ..
        } => {
            assert_eq!(
                *subject_slot,
                Some(0),
                "gate binds to the declared target slot 0"
            );
            let TargetFilter::Typed(tf) = filter else {
                panic!("gate filter must be Typed, got {filter:?}");
            };
            assert!(
                tf.properties.contains(&FilterProp::IsChosenCreatureType),
                "gate must carry IsChosenCreatureType, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected TargetMatchesFilter gate, got {other:?}"),
    }

    // (d) counters + deathtouch target the ATTACKING token, not SelfRef.
    match &*ab.effect {
        Effect::PutCounter { target, .. } => {
            let TargetFilter::Typed(tf) = target else {
                panic!("PutCounter must target a Typed attacking token, got {target:?}");
            };
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::Attacking { .. }))
                    && tf.properties.contains(&FilterProp::Token),
                "PutCounter target must be an attacking token, got {:?}",
                tf.properties
            );
            assert_ne!(
                *target,
                TargetFilter::SelfRef,
                "must NOT target the enchantment"
            );
        }
        other => panic!("expected PutCounter effect, got {other:?}"),
    }
    // Deathtouch grant rides the sub_ability, keyed to the parent (counter) target.
    let sub = ab.sub_ability.as_deref().expect("deathtouch sub_ability");
    match &*sub.effect {
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => {
            assert_eq!(
                *target,
                Some(TargetFilter::ParentTarget),
                "deathtouch grant binds to the counter target (ParentTarget)"
            );
            assert!(
                static_abilities
                    .iter()
                    .any(|s| s.modifications.iter().any(|m| matches!(
                        m,
                        engine::types::ability::ContinuousModification::AddKeyword {
                            keyword: Keyword::Deathtouch,
                            ..
                        }
                    ))),
                "sub_ability must grant Deathtouch"
            );
        }
        other => panic!("expected GenericEffect deathtouch grant, got {other:?}"),
    }
}
