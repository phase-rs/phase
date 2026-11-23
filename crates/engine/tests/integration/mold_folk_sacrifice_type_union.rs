//! Mold Folk — "Mold Harvest — {1}, Sacrifice another creature or an artifact:
//! Put a +1/+1 counter on this creature."
//!
//! CR 205.2a (a cost filter may be a two-leg card-TYPE union) + CR 601.2h (the
//! activation cost is paid with permanents matching that filter) + CR 701.21a
//! (sacrifice).
//!
//! The right conjunct of the union leads with an indefinite article, and the
//! shared type-phrase grammar deliberately leaves that tail as remainder — the
//! same surface is an elided-verb clause elsewhere ("you control a land creature
//! or a land entered the battlefield under your control this turn", Earth Rumble Wrestlers). The
//! sacrifice-cost parser now opts into the union reading via
//! `fold_article_led_type_union`, so the cost's filter is
//! `Or[Creature+Another, Artifact]` rather than `Creature+Another` alone.
//!
//! Note the ASYMMETRY, which is the rules-load-bearing part: "another" scopes only
//! the LEFT conjunct. The right one carries its own determiner ("an artifact"), so
//! it is not "another artifact" and the source may pay with ITSELF once it is an
//! artifact. Official rulings — Elite Headhunter (2019-10-04): "If Elite Headhunter
//! somehow becomes an artifact, you can sacrifice it to pay the cost of its
//! activated ability"; Gut, True Soul Zealot (2022-06-10): "If Gut somehow becomes
//! an artifact, you may sacrifice it to its own ability." The third test below
//! drives exactly that.
//!
//! Before the fix the artifact leg was dropped, so a controller holding only an
//! artifact could not pay a cost the card plainly allows. This drives the real
//! activation pipeline (`GameAction::ActivateAbility` → cost payment → stack →
//! resolution) rather than asserting a parsed filter shape.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, PayCostKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MOLD_FOLK: &str = "Lifelink\n\
     Mold Harvest — {1}, Sacrifice another creature or an artifact: Put a +1/+1 counter on this creature.";

/// The artifact leg is REACHABLE: with no other creature on the battlefield, the
/// only legal sacrifice is the artifact, so the ability can be activated only if
/// the union kept its second leg.
///
/// Revert-failing: with the cost filter collapsed to `Creature+Another` the cost
/// is unpayable here, the ability never resolves, and the source gains no
/// counter while the artifact stays on the battlefield.
#[test]
fn mold_folk_pays_its_cost_by_sacrificing_an_artifact() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Mold Folk", 1, 1, MOLD_FOLK)
        .id();
    let artifact = scenario.add_artifact_from_oracle(P0, "Bone Shard", "").id();
    // CR 602.1a: {1} for the activation cost (everything before the colon).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();

    // CR 601.2h: the artifact is submitted as the cost payment. The engine
    // accepts it only if the cost's filter actually admits an artifact.
    let outcome = runner.activate(source, 0).pay_with(&[artifact]).resolve();
    let state = outcome.state();

    assert_eq!(
        state.objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the ability must resolve, which it can only do if the artifact was a legal sacrifice"
    );
    assert_eq!(
        state.objects[&artifact].zone,
        Zone::Graveyard,
        "the artifact leg of the cost's type union must be sacrificeable"
    );
}

/// PAIRED CONTROL, the leg that already worked: with a creature available and no
/// artifact, the same ability still pays by sacrificing the creature. Proves the
/// union widened the filter rather than replacing one leg with the other.
#[test]
fn mold_folk_still_pays_its_cost_by_sacrificing_a_creature() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Mold Folk", 1, 1, MOLD_FOLK)
        .id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();

    let outcome = runner.activate(source, 0).pay_with(&[fodder]).resolve();
    let state = outcome.state();

    assert_eq!(
        state.objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the creature leg must keep working"
    );
    assert_eq!(
        state.objects[&fodder].zone,
        Zone::Graveyard,
        "the creature was the sacrifice"
    );
    assert_eq!(
        state.objects[&source].zone,
        Zone::Battlefield,
        "\"another\" excludes the source itself"
    );
}

/// CR 205.2a + the Elite Headhunter / Gut rulings: "another" scopes only the LEFT
/// conjunct, so once the source itself is an artifact it satisfies the RIGHT leg
/// and may be sacrificed to its own ability.
///
/// This is the discriminating case for the `Another` scoping, and it is reachable
/// in real games — Liquimetal Coating and Mycosynth Lattice both make a creature
/// an artifact "in addition to its other types". (Not Karn, Silver Golem: that
/// one targets a NONcreature artifact and makes it a creature, the opposite
/// direction.)
///
/// GUARD-FAILING: distribute `FilterProp::Another` onto the article-led right leg
/// (i.e. fold the union *before* applying `another`, as an earlier revision did)
/// and the source matches neither leg — `matches_filter_prop`'s `Another` arm
/// resolves to `!source_is_current_object(..)` on this path — so
/// `find_eligible_sacrifice_targets` returns nothing, the cost is unpayable with
/// nothing else on the battlefield, and the activation is refused.
#[test]
fn mold_folk_may_sacrifice_itself_once_it_is_an_artifact() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Set as a fixture rather than parsed from a type-granting card: the subject
    // here is the COST FILTER, and routing through a continuous effect would put
    // the layer system in the path of a test that is not about it.
    // `as_artifact` REPLACES the creature type; `as_creature` puts it back
    // idempotently and re-syncs the base types, so the fixture is an artifact
    // CREATURE — the "in addition to its other types" state Liquimetal Coating and
    // Mycosynth Lattice produce, and the shape Street Urchin's own 2022-06-10
    // ruling describes ("If your commander is an artifact creature, you may
    // sacrifice it to pay the cost of this ability"). A pure artifact would
    // discriminate the same way but leave the both-types arm unexercised.
    let source = {
        let mut b = scenario.add_creature_from_oracle(P0, "Mold Folk", 1, 1, MOLD_FOLK);
        b.as_artifact();
        b.as_creature();
        b.id()
    };
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();

    let core = &runner.state().objects[&source].card_types.core_types;
    assert!(
        core.contains(&CoreType::Artifact) && core.contains(&CoreType::Creature),
        "reach-guard: the source must be an artifact CREATURE, or this proves \
         something narrower than the rulings describe: {core:?}"
    );

    // Nothing else is on the battlefield, so the ONLY legal payment is the source
    // itself via the artifact leg.
    let outcome = runner.activate(source, 0).pay_with(&[source]).resolve();
    let state = outcome.state();
    assert_eq!(
        state.objects[&source].zone,
        Zone::Graveyard,
        "the source satisfies the article-led artifact leg and may pay with itself"
    );
}

/// COVERAGE GUARD for `Another` on the LEFT (creature) leg.
///
/// The three tests above all pass even if `Another` is stripped from the creature
/// leg: two of them submit a DIFFERENT permanent as the payment, and the third
/// pays through the artifact leg, which never carried `Another` to begin with.
/// Nothing above discriminates the property, so this drives the one case that
/// does — Mold Folk alone on the battlefield as a plain, NON-artifact creature.
///
/// CR 601.2h: with no other permanent to sacrifice and the source excluded from
/// the only leg it could match, the cost is unpayable — "Unpayable costs can't
/// be paid" — so the activation cannot be completed, and CR 733.1 reverses the
/// entire action, leaving the board exactly as it was.
///
/// Revert-failing: remove `FilterProp::Another` from the creature leg and the
/// source satisfies that leg itself, the cost becomes payable, and the
/// activation below is accepted instead of rejected.
#[test]
fn mold_folk_cannot_sacrifice_itself_while_it_is_only_a_creature() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Mold Folk", 1, 1, MOLD_FOLK)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();

    let core = &runner.state().objects[&source].card_types.core_types;
    assert!(
        core.contains(&CoreType::Creature) && !core.contains(&CoreType::Artifact),
        "reach-guard: the source must be a plain creature, or this tests the \
         artifact leg instead of the creature leg: {core:?}"
    );

    // The mana is available and the ability is otherwise activatable, so the ONLY
    // thing that can refuse this is the sacrifice filter excluding the source.
    let result = runner.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: 0,
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    });
    assert!(
        result.is_err(),
        "\"another\" must exclude the source from the creature leg, leaving the \
         cost unpayable with nothing else on the battlefield, got {result:?}"
    );

    let state = runner.state();
    assert_eq!(
        state.objects[&source].zone,
        Zone::Battlefield,
        "the refused activation must leave the source on the battlefield"
    );
    assert_eq!(
        state.objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "the ability must not have resolved"
    );
}

const GARGADON: &str = "Suspend 10—{R}\nSacrifice an artifact, creature, or land: Remove a time counter from this card. Activate only if this card is suspended.";

fn suspended_gargadon() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_exile(P0, "Greater Gargadon", 9, 7)
        .from_oracle_text_with_keywords(&["Suspend"], GARGADON)
        .id();
    scenario.with_counter(source, CounterType::Time, 2);
    (scenario, source)
}

fn gargadon_activation(runner: &GameRunner, source: ObjectId) -> GameAction {
    let card = &runner.state().objects[&source];
    assert_eq!(card.zone, Zone::Exile);
    assert_eq!(card.counters.get(&CounterType::Time), Some(&2));
    GameAction::ActivateAbility {
        source_id: source,
        ability_index: card
            .abilities
            .iter()
            .position(|a| matches!(*a.effect, Effect::RemoveCounter { .. }))
            .expect("printed counter-removal ability"),
        payment_mode: CastPaymentMode::Auto,
    }
}

fn assert_sacrifice_prompt(runner: &GameRunner, eligible: &[ObjectId]) {
    let WaitingFor::PayCost {
        player,
        kind: PayCostKind::Sacrifice,
        choices,
        min_count,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a sacrifice choice, got {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    let mut actual = choices.clone();
    actual.sort();
    let mut expected = eligible.to_vec();
    expected.sort();
    assert_eq!(actual, expected);
}

fn finish_gargadon_payment(
    runner: &mut GameRunner,
    source: ObjectId,
    selected: ObjectId,
    battlefield: &[ObjectId],
) {
    let owner = runner.state().objects[&selected].owner;
    runner
        .act(GameAction::SelectCards {
            cards: vec![selected],
        })
        .unwrap();
    let state = runner.state();
    assert_eq!(state.objects[&selected].zone, Zone::Graveyard);
    assert!(state.players[owner.0 as usize]
        .graveyard
        .contains(&selected));
    for id in battlefield.iter().filter(|id| **id != selected) {
        assert_eq!(state.objects[id].zone, Zone::Battlefield);
    }
    assert_eq!(state.objects[&source].zone, Zone::Exile);
    assert_eq!(
        state.objects[&source].counters.get(&CounterType::Time),
        Some(&2)
    );
    assert_eq!(state.stack.len(), 1);
    let entry = state.stack.back().unwrap();
    assert_eq!((entry.source_id, entry.controller), (source, P0));
    assert!(matches!(
        entry.kind,
        StackEntryKind::ActivatedAbility { .. }
    ));
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn greater_gargadon_pays_with_any_one_of_its_three_printed_types() {
    let (mut scenario, source) = suspended_gargadon();
    let ring = scenario
        .add_artifact_from_oracle(P0, "Sol Ring", "{T}: Add {C}{C}.")
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let thopter = scenario
        .add_creature(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .id();
    let enchantment = scenario
        .add_enchantment_from_oracle(P0, "Enchantment", "")
        .id();
    let opponent = scenario.add_creature(P1, "Opponent creature", 2, 2).id();
    let mut runner = scenario.build();
    let eligible = [ring, bears, mountain, thopter];
    for (id, types) in [
        (ring, vec![CoreType::Artifact]),
        (bears, vec![CoreType::Creature]),
        (mountain, vec![CoreType::Land]),
        (thopter, vec![CoreType::Artifact, CoreType::Creature]),
        (enchantment, vec![CoreType::Enchantment]),
        (opponent, vec![CoreType::Creature]),
    ] {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.card_types.core_types.len(), types.len());
        assert!(types.iter().all(|t| obj.card_types.core_types.contains(t)));
    }
    runner
        .act(
            public_activations(&runner, source)
                .pop()
                .expect("published sacrifice ability"),
        )
        .unwrap();
    assert_sacrifice_prompt(&runner, &eligible);
    for cards in [
        vec![enchantment],
        vec![opponent],
        vec![],
        vec![ring, mountain],
    ] {
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(runner.act(GameAction::SelectCards { cards }).is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
    }
    for chosen in eligible {
        let mut paying = GameRunner::from_state(runner.state().clone());
        finish_gargadon_payment(
            &mut paying,
            source,
            chosen,
            &[ring, bears, mountain, thopter, enchantment, opponent],
        );
    }
}

#[test]
fn greater_gargadon_needs_only_one_matching_permanent() {
    for kind in [
        None,
        Some(CoreType::Artifact),
        Some(CoreType::Creature),
        Some(CoreType::Land),
    ] {
        let (mut scenario, source) = suspended_gargadon();
        let enchantment = scenario
            .add_enchantment_from_oracle(P0, "Enchantment", "")
            .id();
        let opponent = scenario.add_creature(P1, "Opponent creature", 2, 2).id();
        let selected = kind.map(|kind| match kind {
            CoreType::Artifact => scenario
                .add_artifact_from_oracle(P0, "Sol Ring", "{T}: Add {C}{C}.")
                .id(),
            CoreType::Creature => scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
            CoreType::Land => scenario.add_basic_land(P0, ManaColor::Red),
            _ => unreachable!(),
        });
        let mut runner = scenario.build();
        let action = gargadon_activation(&runner, source);
        if let Some(selected) = selected {
            runner
                .act(
                    public_activations(&runner, source)
                        .pop()
                        .expect("published sacrifice ability"),
                )
                .unwrap();
            assert_sacrifice_prompt(&runner, &[selected]);
            finish_gargadon_payment(
                &mut runner,
                source,
                selected,
                &[selected, opponent, enchantment],
            );
        } else {
            assert!(public_activations(&runner, source).is_empty());
            assert!(runner.act(action).is_err());
            assert!(runner.state().stack.is_empty());
            assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
            assert_eq!(runner.state().objects[&enchantment].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&opponent].zone, Zone::Battlefield);
        }
    }
}

#[test]
fn greater_gargadon_sacrifices_a_controlled_permanent_to_its_owners_graveyard() {
    let (mut scenario, source) = suspended_gargadon();
    let theirs = scenario.add_creature(P1, "Their creature", 2, 2).id();
    let ours = scenario.add_creature(P0, "Our creature", 2, 2).id();
    let mut runner = scenario.build();
    for (id, controller) in [(theirs, P0), (ours, P1)] {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.base_controller = Some(controller);
        object.controller = controller;
    }
    runner
        .act(
            public_activations(&runner, source)
                .pop()
                .expect("published sacrifice ability"),
        )
        .unwrap();
    assert_sacrifice_prompt(&runner, &[theirs]);
    assert_eq!(runner.state().objects[&theirs].owner, P1);
    finish_gargadon_payment(&mut runner, source, theirs, &[ours, theirs]);
}

fn public_activations(runner: &GameRunner, source: ObjectId) -> Vec<GameAction> {
    let (_, _, mut grouped) = engine::ai_support::legal_actions_for_viewer(runner.state(), P0);
    grouped.remove(&source).unwrap_or_default().into_iter()
        .filter(|action| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == source))
        .collect()
}

#[test]
fn greater_gargadon_activates_from_its_exile_row_after_suspend() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let gargadon = scenario
        .add_creature_to_hand_from_oracle(P0, "Greater Gargadon", 9, 7, GARGADON)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 9,
            shards: vec![engine::types::mana::ManaCostShard::Red],
        })
        .id();
    let mut runner = scenario.build();
    let hand = public_activations(&runner, gargadon);
    assert_eq!(hand.len(), 1, "select Suspend on the card in hand");
    let GameAction::ActivateAbility {
        ability_index: suspend,
        ..
    } = hand[0]
    else {
        panic!("main still models suspend as an activation (CR 116.2f gap)");
    };
    runner
        .activate(gargadon, suspend)
        .target_object(gargadon)
        .resolve();
    assert_eq!(runner.state().objects[&gargadon].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&gargadon].counters[&CounterType::Time],
        10
    );
    let exile = public_activations(&runner, gargadon);
    assert_eq!(
        exile.len(),
        1,
        "the exile viewer must offer the printed sacrifice ability"
    );
    assert_ne!(hand[0], exile[0]);
    runner.act(exile[0].clone()).unwrap();
    assert_sacrifice_prompt(&runner, &[mountain]);
    runner
        .act(GameAction::SelectCards {
            cards: vec![mountain],
        })
        .unwrap();
    assert_eq!(runner.state().objects[&mountain].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&gargadon].counters[&CounterType::Time],
        10
    );
    assert_eq!(runner.state().stack.len(), 1);
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&gargadon].counters[&CounterType::Time],
        9
    );
}

#[test]
fn suspended_activation_checks_this_face_its_keyword_and_its_time_counter() {
    let (mut scenario, source) = suspended_gargadon();
    let other = scenario
        .add_creature_to_exile(P0, "Greater Gargadon", 9, 7)
        .from_oracle_text_with_keywords(&["Suspend"], GARGADON)
        .id();
    scenario.with_counter(other, CounterType::Time, 4);
    scenario.add_basic_land(P0, ManaColor::Red);
    let baseline = scenario.build();
    let action = public_activations(&baseline, source).pop().unwrap();
    for change in [
        "last counter",
        "lost suspend",
        "face-down visible",
        "wrong owner",
        "wrong zone",
    ] {
        let mut runner = GameRunner::from_state(baseline.state().clone());
        if change == "wrong zone" {
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                source,
                Zone::Graveyard,
                &mut vec![],
            );
        } else {
            let card = runner.state_mut().objects.get_mut(&source).unwrap();
            match change {
                "last counter" => {
                    card.counters.remove(&CounterType::Time);
                }
                "lost suspend" => {
                    card.keywords.clear();
                    card.base_keywords.clear();
                }
                "face-down visible" => {
                    card.face_down = true;
                    card.foretold = true;
                }
                "wrong owner" => {
                    card.owner = P1;
                }
                _ => unreachable!(),
            }
        }
        assert!(public_activations(&runner, source).is_empty(), "{change}");
        assert_eq!(public_activations(&runner, other).len(), 1, "{change}");
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(runner.act(action.clone()).is_err(), "{change}");
        assert_eq!(
            serde_json::to_value(runner.state()).unwrap(),
            before,
            "{change}"
        );
        assert!(
            !engine::ai_support::activation_block_reasons_for_viewer(runner.state(), P0)
                .contains_key(&source),
            "{change}"
        );
    }
}

#[test]
fn paid_sacrifice_removes_a_counter_only_from_the_announced_incarnation() {
    let (mut scenario, source) = suspended_gargadon();
    let sibling = scenario
        .add_creature_to_exile(P0, "Greater Gargadon", 9, 7)
        .from_oracle_text_with_keywords(&["Suspend"], GARGADON)
        .id();
    scenario.with_counter(sibling, CounterType::Time, 4);
    let land = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    runner
        .act(public_activations(&runner, source).pop().unwrap())
        .unwrap();
    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .unwrap();
    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert_eq!(runner.state().stack.len(), 1);
    for leave in [false, true] {
        let mut pending = GameRunner::from_state(runner.state().clone());
        if leave {
            // Pull from Eternity can remove a face-up source before its ability resolves.
            let old = pending.state().objects[&source].incarnation;
            for zone in [Zone::Graveyard, Zone::Exile] {
                engine::game::zones::move_to_zone(pending.state_mut(), source, zone, &mut vec![]);
            }
            let reentered = pending.state_mut().objects.get_mut(&source).unwrap();
            assert_ne!(reentered.incarnation, old);
            reentered.counters.insert(CounterType::Time, 7);
        }
        pending.resolve_top();
        assert_eq!(
            pending.state().objects[&source].counters[&CounterType::Time],
            if leave { 7 } else { 1 }
        );
        assert_eq!(
            pending.state().objects[&sibling].counters[&CounterType::Time],
            4
        );
        assert_eq!(pending.state().objects[&land].zone, Zone::Graveyard);
    }
}

const TORRENT: &str = "Flying\nWhenever this creature attacks, tap all creatures defending player controls.\n{3}{B/G}{B/G}: Put this card from exile onto the battlefield tapped. Activate only as a sorcery.";

#[test]
fn torrent_elemental_exile_offer_follows_affordability_owner_and_timing() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_exile(P0, "Torrent Elemental", 3, 5)
        .from_oracle_text_with_keywords(&["Flying"], TORRENT)
        .id();
    let theirs = scenario
        .add_creature_to_exile(P1, "Torrent Elemental", 3, 5)
        .from_oracle_text_with_keywords(&["Flying"], TORRENT)
        .id();
    let forests: Vec<_> = (0..5)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    let fog = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Fog",
            true,
            "Prevent all combat damage that would be dealt this turn.",
        )
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 0,
            shards: vec![engine::types::mana::ManaCostShard::Green],
        })
        .id();
    let mut runner = scenario.build();
    let action = public_activations(&runner, source)
        .pop()
        .expect("exile offers Torrent Elemental");
    let mut response = GameRunner::from_state(runner.state().clone());
    response.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Green,
        ObjectId(0),
        false,
        vec![],
    ));
    let card_id = response.state().objects[&fog].card_id;
    response
        .act(GameAction::CastSpell {
            object_id: fog,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert_eq!(response.state().stack.len(), 1);
    assert!(public_activations(&response, source).is_empty());
    assert!(
        !engine::ai_support::activation_block_reasons_for_viewer(response.state(), P0)
            .contains_key(&source)
    );
    assert!(response.act(action.clone()).is_err());
    assert!(public_activations(&runner, theirs).is_empty());
    for land in &forests {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }
    assert!(public_activations(&runner, source).is_empty());
    assert!(
        engine::ai_support::activation_block_reasons_for_viewer(runner.state(), P0)
            .contains_key(&source),
        "insufficient mana"
    );
    assert!(
        !engine::ai_support::activation_block_reasons_for_viewer(runner.state(), P1)
            .contains_key(&source)
    );
    for land in &forests {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = false;
    }
    runner.state_mut().phase = Phase::Upkeep;
    assert!(public_activations(&runner, source).is_empty());
    assert!(
        !engine::ai_support::activation_block_reasons_for_viewer(runner.state(), P0)
            .contains_key(&source)
    );
    assert!(runner.act(action.clone()).is_err());
    runner.state_mut().phase = Phase::PreCombatMain;
    runner
        .act(public_activations(&runner, source).pop().unwrap())
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert!(runner.state().objects[&source].tapped);
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Exile);
}

#[test]
fn carrionette_offers_its_graveyard_ability_and_exiles_the_chosen_creature() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario.add_creature_to_graveyard(P0, "Carrionette", 1, 1)
        .from_oracle_text_with_keywords(&[], "{2}{B}{B}: Exile this card and target creature unless that creature's controller pays {2}. Activate only if this card is in your graveyard.").id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let other = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    let mut targetless = GameRunner::from_state(runner.state().clone());
    for creature in [bears, other] {
        engine::game::zones::move_to_zone(
            targetless.state_mut(),
            creature,
            Zone::Graveyard,
            &mut vec![],
        );
    }
    assert!(public_activations(&targetless, card).is_empty());
    assert!(
        !engine::ai_support::activation_block_reasons_for_viewer(targetless.state(), P0)
            .contains_key(&card)
    );

    runner
        .act(
            public_activations(&runner, card)
                .pop()
                .expect("Carrionette in graveyard"),
        )
        .unwrap();
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(bears)),
        })
        .unwrap();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
}

#[test]
fn pithing_needle_stops_the_printed_exile_ability() {
    let (mut scenario, source) = suspended_gargadon();
    let needle = scenario.add_artifact_from_oracle(P1, "Pithing Needle", "As this artifact enters, choose a card name.\nActivated abilities of sources with the chosen name can't be activated unless they're mana abilities.").id();
    scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    assert_eq!(public_activations(&runner, source).len(), 1);
    runner
        .state_mut()
        .objects
        .get_mut(&needle)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::CardName(
            "Greater Gargadon".into(),
        ));
    assert!(public_activations(&runner, source).is_empty());
    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::RemoveCounter { .. }))
        .unwrap();
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto
        })
        .is_err());
    engine::game::zones::move_to_zone(runner.state_mut(), needle, Zone::Graveyard, &mut vec![]);
    assert_eq!(public_activations(&runner, source).len(), 1);
}

#[test]
fn split_second_suppresses_the_exiled_sacrifice_but_preserves_mana_abilities() {
    use engine::types::ability::TargetRef;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaCost, ManaCostShard};
    let (mut scenario, source) = suspended_gargadon();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Sudden Shock", true, "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nSudden Shock deals 2 damage to any target.")
        .with_keyword(Keyword::SplitSecond)
        .with_mana_cost(ManaCost::Cost{generic:1, shards:vec![ManaCostShard::Red]}).id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let elves = scenario
        .add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, "{T}: Add {G}.")
        .id();
    let mut runner = scenario.build();
    let act = public_activations(&runner, source).pop().unwrap();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![bears],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bears)),
            })
            .unwrap();
    }
    assert_eq!(runner.state().stack.len(), 1);
    assert!(public_activations(&runner, source).is_empty());
    assert!(runner.act(act).is_err());
    let mana = engine::ai_support::legal_actions_for_viewer(runner.state(), P0)
        .2
        .into_values()
        .flatten()
        .find(|a| matches!(a, GameAction::TapLandForMana { .. }))
        .expect("the untapped Mountain can still produce mana");
    runner.act(mana).unwrap();
    assert_eq!(runner.state().players[0].mana_pool.total(), 1);
    // CR 702.61b + CR 605.3b: a mana ability submitted through ActivateAbility
    // resolves before any split-second gate.
    runner
        .act(GameAction::ActivateAbility {
            source_id: elves,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("split second does not stop a mana ability");
    assert_eq!(runner.state().players[0].mana_pool.total(), 2);
    runner.resolve_top();
    assert_eq!(public_activations(&runner, source).len(), 1);
}

#[test]
fn an_independent_exile_cast_and_the_sacrifice_are_separate_choices() {
    use engine::types::mana::{ManaCost, ManaCostShard};
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountains: Vec<_> = (0..12)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    let card = scenario
        .add_creature_to_hand_from_oracle(P0, "Greater Gargadon", 9, 7, GARGADON)
        .with_mana_cost(ManaCost::Cost {
            generic: 9,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let impulse = scenario.add_spell_to_hand_from_oracle(P0, "Reckless Impulse", false,
        "Exile the top two cards of your library. Until the end of your next turn, you may play those cards.")
        .with_mana_cost(ManaCost::Cost { generic:1,shards:vec![ManaCostShard::Red] }).id();
    let mut runner = scenario.build();
    engine::game::zones::move_to_library_position(runner.state_mut(), card, true, &mut vec![]);
    runner.cast(impulse).resolve();
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
    // Set the counter axis alongside Impulse's actual play permission.
    runner
        .state_mut()
        .objects
        .get_mut(&card)
        .unwrap()
        .counters
        .insert(CounterType::Time, 4);
    let (_, _, grouped) = engine::ai_support::legal_actions_for_viewer(runner.state(), P0);
    let row = &grouped[&card];
    let cast = row
        .iter()
        .find(|a| matches!(a, GameAction::CastSpell { .. }))
        .expect("Impulse cast")
        .clone();
    let activation = row
        .iter()
        .find(|a| matches!(a, GameAction::ActivateAbility { .. }))
        .expect("printed sacrifice")
        .clone();
    let mut casting = GameRunner::from_state(runner.state().clone());
    casting.act(cast).unwrap();
    assert_eq!(casting.state().objects[&card].zone, Zone::Stack);
    assert!(mountains
        .iter()
        .all(|id| casting.state().objects[id].zone == Zone::Battlefield));
    runner.act(activation).unwrap();
    assert_sacrifice_prompt(&runner, &mountains);
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
}
