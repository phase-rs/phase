//! CR 601.2a + CR 601.2b: the caster announces which graveyard permission a
//! cast uses. Muldrotha, the Gravetide (2020-11-10 ruling): "If multiple
//! effects allow you to play a card from your graveyard, you must announce
//! which permission you're using as you begin to play the card."
//!
//! Every board here has two or more permissions that could authorize the same
//! cast. Each test chooses EACH permission through the production casting menu
//! (`ChooseCastingVariant`) and asserts what that choice commits the cast to:
//! whose per-turn slot is spent, whose extra cost or counter rider is charged,
//! and what mana is paid. A single-permission board still casts without a
//! prompt.

use super::blitz_em_dash_graveyard_cast::{
    add_bears_to_graveyard, add_exploration_broodship, add_mana, add_permission_host,
    add_permission_source, blitz_keyword, caldaia_blitz, cast_from_graveyard, creature_permission,
    fill_mana, mycosynth_muldrotha_board, offered_cast, pay_offered_costs, sabin_permission_board,
    BOON_SATYR, BROODSHIP_STATIONED, LEONARDO, LURRUS, MULDROTHA, RIVETEERS_DECOY,
};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    AnnouncedGraveyardPermission, CastPaymentMode, CastingVariant, CastingVariantChoiceOption,
    GrantDigest, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::statics::CastFrequency;
use engine::types::zones::Zone;

/// The casting method of a menu option, by name.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Method {
    Printed,
    Blitz,
    Bestow,
}

fn method_of(option: &CastingVariantChoiceOption) -> Option<Method> {
    match option.variant {
        CastingVariant::GraveyardPermission { .. } => Some(Method::Printed),
        CastingVariant::Blitz => Some(Method::Blitz),
        CastingVariant::Bestow => Some(Method::Bestow),
        _ => None,
    }
}

fn announcement(option: &CastingVariantChoiceOption) -> Option<&AnnouncedGraveyardPermission> {
    option
        .authority
        .as_ref()
        .map(|authority| &authority.announcement)
}

/// The menu option for `method` announced under `source` (and `slot`, when
/// given), or `None`.
fn option_index(
    options: &[CastingVariantChoiceOption],
    method: Method,
    source: ObjectId,
    slot: Option<CoreType>,
) -> Option<usize> {
    options.iter().position(|option| {
        method_of(option) == Some(method)
            && announcement(option).is_some_and(|announced| {
                announced.permission.source == source
                    && slot.is_none_or(|slot| announced.slot_type == Some(slot))
            })
    })
}

/// Start casting `id` from the graveyard with `payment_mode` and return the
/// announcement menu it asks.
fn menu(
    runner: &mut GameRunner,
    id: ObjectId,
    payment_mode: CastPaymentMode,
) -> Vec<CastingVariantChoiceOption> {
    let card_id = runner.state().objects[&id].card_id;
    let waiting = runner
        .act(GameAction::CastSpell {
            object_id: id,
            card_id,
            targets: vec![],
            payment_mode,
        })
        .expect("the graveyard cast starts")
        .waiting_for;
    match waiting {
        WaitingFor::CastingVariantChoice { options, .. } => options,
        other => panic!("two permissions must ask for the announcement, got {other:?}"),
    }
}

/// Cast `id` from the graveyard choosing `method` under `source` (and `slot`),
/// then enchant the first legal creature and pay each offered cost. Asserts the
/// spell reaches the stack.
fn cast_announcing(
    runner: &mut GameRunner,
    id: ObjectId,
    method: Method,
    source: ObjectId,
    slot: Option<CoreType>,
) {
    let options = menu(runner, id, CastPaymentMode::Auto);
    let index = option_index(&options, method, source, slot).unwrap_or_else(|| {
        panic!("the menu must offer {method:?} under {source:?} ({slot:?}), got {options:?}")
    });
    runner
        .act(GameAction::ChooseCastingVariant { index })
        .expect("choosing an offered announcement is legal");
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
        runner
            .choose_first_legal_target()
            .expect("a legal creature to enchant");
    }
    pay_offered_costs(runner);
    assert_eq!(
        runner.state().objects[&id].zone,
        Zone::Stack,
        "the {method:?} cast under {source:?} completes, waiting for {:?}",
        runner.state().waiting_for
    );
}

fn object_named(runner: &GameRunner, name: &str) -> ObjectId {
    runner
        .state()
        .objects
        .values()
        .find(|obj| obj.name == name)
        .unwrap_or_else(|| panic!("{name} is on the board"))
        .id
}

fn per_type_used(runner: &GameRunner) -> Vec<(ObjectId, CoreType)> {
    let mut used: Vec<_> = runner
        .state()
        .graveyard_cast_permissions_used_per_type
        .iter()
        .copied()
        .collect();
    used.sort_by_key(|(source, slot)| (source.0, format!("{slot:?}")));
    used
}

fn once_used(runner: &GameRunner, source: ObjectId) -> bool {
    runner
        .state()
        .graveyard_cast_permissions_used
        .contains(&source)
}

/// Add `core` to the graveyard card's current and base type lines (the
/// graveyard builder seeds only Creature).
fn add_core_type(runner: &mut GameRunner, id: ObjectId, core: CoreType) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    for types in [
        &mut obj.card_types.core_types,
        &mut obj.base_card_types.core_types,
    ] {
        if !types.contains(&core) {
            types.push(core);
        }
    }
}

// --- Sabin, Master Monk: its own permission beside Muldrotha --------------

/// Sabin's own "using its blitz ability" permission beside Muldrotha, the
/// common board. Both admit the blitz, so the player announces one: Sabin's
/// own spends nothing, Muldrotha's spends its creature slot. Each pays blitz's
/// {2}{R}{R} and the discard.
#[test]
fn sabin_blitz_announces_its_own_permission_or_muldrothas() {
    for choose_own in [true, false] {
        let (mut runner, sabin) = sabin_permission_board(true, true, false);
        let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
        let source = if choose_own { sabin } else { muldrotha };
        cast_announcing(&mut runner, sabin, Method::Blitz, source, None);
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            4,
            "blitz's {{2}}{{R}}{{R}} is paid (own {choose_own})"
        );
        assert_eq!(
            runner.state().players[0].hand.len(),
            0,
            "blitz's discard is paid (own {choose_own})"
        );
        let expected = if choose_own {
            vec![]
        } else {
            vec![(muldrotha, CoreType::Creature)]
        };
        assert_eq!(
            per_type_used(&runner),
            expected,
            "only the announced permission's slot is spent (own {choose_own})"
        );
    }
}

/// Sabin beside its own permission, Muldrotha and Exploration Broodship: all
/// three blitz announcements are offered, and Broodship's charges its land.
#[test]
fn sabin_blitz_offers_each_of_three_permissions() {
    let (mut runner, sabin) = sabin_permission_board(true, true, true);
    let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
    let broodship = object_named(&runner, "Exploration Broodship");
    let options = menu(
        &mut GameRunner::from_state(runner.state().clone()),
        sabin,
        CastPaymentMode::Auto,
    );
    for source in [sabin, muldrotha, broodship] {
        assert!(
            option_index(&options, Method::Blitz, source, None).is_some(),
            "blitz under {source:?} is offered, got {options:?}"
        );
    }
    let land = object_named(&runner, "Mountain");
    cast_announcing(&mut runner, sabin, Method::Blitz, broodship, None);
    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert!(once_used(&runner, broodship));
    assert!(per_type_used(&runner).is_empty());
}

// --- Muldrotha vs Leonardo, Sewer Samurai (Riveteers Decoy) ---------------

/// Ashnod's Altar: "Sacrifice a creature: Add {C}{C}."
const ASHNODS_ALTAR: &str = "Sacrifice a creature: Add {C}{C}.";

const KRARK_CLAN_IRONWORKS: &str = "Sacrifice an artifact: Add {C}{C}.";

fn decoy_board(with_muldrotha: bool, with_leonardo: bool) -> (GameRunner, ObjectId) {
    decoy_board_with(with_muldrotha, with_leonardo, false)
}

fn decoy_board_with(
    with_muldrotha: bool,
    with_leonardo: bool,
    altar: bool,
) -> (GameRunner, ObjectId) {
    let decoy_blitz = blitz_keyword(&parse_oracle_text(
        RIVETEERS_DECOY,
        "Riveteers Decoy",
        &["Blitz".into()],
        &["Creature".into()],
        &["Human".into(), "Warrior".into()],
    ));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if with_muldrotha {
        add_permission_source(
            &mut scenario,
            "Muldrotha, the Gravetide",
            MULDROTHA,
            &["Elemental", "Avatar"],
        );
    }
    if with_leonardo {
        add_permission_source(
            &mut scenario,
            "Leonardo, Sewer Samurai",
            LEONARDO,
            &["Mutant", "Ninja", "Turtle", "Samurai"],
        );
    }
    let decoy = scenario
        .add_creature_to_graveyard(P0, "Riveteers Decoy", 3, 1)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(decoy_blitz)
        .id();
    if altar {
        scenario.add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR);
    }
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Green);
    (runner, decoy)
}

/// Leonardo is unlimited but gives the creature a finality counter; Muldrotha
/// spends its creature slot. Announcing Leonardo pays blitz's {3}{G}, spends
/// no slot and the Decoy enters with a finality counter; announcing Muldrotha
/// spends the creature slot and adds no counter.
#[test]
fn decoy_blitz_announces_leonardo_or_muldrotha() {
    for choose_leonardo in [true, false] {
        let (mut runner, decoy) = decoy_board(true, true);
        let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
        let leonardo = object_named(&runner, "Leonardo, Sewer Samurai");
        let source = if choose_leonardo { leonardo } else { muldrotha };
        cast_announcing(&mut runner, decoy, Method::Blitz, source, None);
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            4,
            "blitz's {{3}}{{G}} is paid (leonardo {choose_leonardo})"
        );
        let expected = if choose_leonardo {
            vec![]
        } else {
            vec![(muldrotha, CoreType::Creature)]
        };
        assert_eq!(per_type_used(&runner), expected);
        runner.resolve_top();
        let entered = &runner.state().objects[&decoy];
        assert_eq!(entered.zone, Zone::Battlefield);
        assert_eq!(
            entered.counters.get(&CounterType::Finality).copied(),
            choose_leonardo.then_some(1),
            "only Leonardo's announcement adds its finality counter, counters: {:?}",
            entered.counters
        );
    }
}

/// Reach controls on the exact board: each permission alone blitzes the same
/// Decoy with no announcement to make (a sole permission is used without a
/// prompt).
#[test]
fn decoy_blitz_under_one_permission_needs_no_announcement() {
    for (muldrotha, leonardo) in [(true, false), (false, true)] {
        let (mut runner, decoy) = decoy_board(muldrotha, leonardo);
        let waiting = cast_from_graveyard(&mut runner, decoy).expect("the cast starts");
        assert!(
            !matches!(waiting, WaitingFor::CastingVariantChoice { .. }),
            "one permission: no announcement menu (muldrotha {muldrotha}), got {waiting:?}"
        );
    }
}

/// CR 601.2b + CR 614.1c: the counter rider is fixed when the cast is
/// announced. Announce Leonardo, then sacrifice Leonardo to Ashnod's Altar for
/// mana during payment: the Decoy still enters with the finality counter, per
/// the Intrepid Paleontologist ruling (the permission's source "doesn't need
/// to stay under your control throughout that process").
#[test]
fn leonardos_counter_applies_after_it_is_sacrificed_mid_payment() {
    let (mut runner, decoy) = decoy_board_with(true, true, true);
    let leonardo = object_named(&runner, "Leonardo, Sewer Samurai");
    let altar = object_named(&runner, "Ashnod's Altar");
    let options = menu(&mut runner, decoy, CastPaymentMode::Manual);
    let index = option_index(&options, Method::Blitz, leonardo, None).expect("blitz via Leonardo");
    runner
        .act(GameAction::ChooseCastingVariant { index })
        .expect("announcing Leonardo is legal");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: 0,
        })
        .expect("Ashnod's Altar is activatable during payment");
    runner
        .act(GameAction::SelectCards {
            cards: vec![leonardo],
        })
        .expect("sacrificing Leonardo for mana is legal");
    runner
        .act(GameAction::PassPriority)
        .expect("committing the payment completes the cast");
    assert_eq!(runner.state().objects[&leonardo].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Stack);
    assert!(
        per_type_used(&runner).is_empty(),
        "Muldrotha's slot stays free"
    );
    runner.resolve_top();
    let entered = &runner.state().objects[&decoy];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(
        entered.counters.get(&CounterType::Finality).copied(),
        Some(1),
        "Leonardo's rider was latched at announcement, counters: {:?}",
        entered.counters
    );
}

// --- Muldrotha vs Exploration Broodship ----------------------------------

fn guardian_board(
    with_muldrotha: bool,
    with_broodship: bool,
    land: bool,
) -> (GameRunner, ObjectId) {
    guardian_board_with(with_muldrotha, with_broodship, land, false)
}

fn guardian_board_with(
    with_muldrotha: bool,
    with_broodship: bool,
    land: bool,
    ironworks: bool,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if with_broodship {
        add_exploration_broodship(&mut scenario, BROODSHIP_STATIONED);
    }
    if with_muldrotha {
        add_permission_source(
            &mut scenario,
            "Muldrotha, the Gravetide",
            MULDROTHA,
            &["Elemental", "Avatar"],
        );
    }
    if land {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let guardian = scenario
        .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(caldaia_blitz())
        .id();
    if ironworks {
        scenario.add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KRARK_CLAN_IRONWORKS);
    }
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    fill_mana(&mut runner, ManaType::Green);
    (runner, guardian)
}

/// CR 601.2a + CR 601.2f: announcing Broodship charges its land sacrifice and
/// spends its once-per-turn slot; announcing Muldrotha charges no land and
/// spends its creature slot. Both are listed with their own authority, so each
/// was priced under its own permission (the inner offer is bound to it).
#[test]
fn guardian_blitz_announces_broodship_or_muldrotha() {
    for choose_broodship in [true, false] {
        let (mut runner, guardian) = guardian_board(true, true, true);
        let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
        let broodship = object_named(&runner, "Exploration Broodship");
        let land = object_named(&runner, "Forest");
        let options = engine::game::casting::current_casting_variant_choice_options(
            runner.state(),
            P0,
            guardian,
        );
        for source in [muldrotha, broodship] {
            assert!(
                option_index(&options, Method::Blitz, source, None).is_some(),
                "blitz under {source:?} got past its own affordability, got {options:?}"
            );
        }
        let source = if choose_broodship {
            broodship
        } else {
            muldrotha
        };
        cast_announcing(&mut runner, guardian, Method::Blitz, source, None);
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            5,
            "blitz's {{2}}{{G}} is paid"
        );
        assert_eq!(
            runner.state().objects[&land].zone == Zone::Graveyard,
            choose_broodship,
            "only Broodship's announcement sacrifices the land"
        );
        assert_eq!(once_used(&runner, broodship), choose_broodship);
        let expected = if choose_broodship {
            vec![]
        } else {
            vec![(muldrotha, CoreType::Creature)]
        };
        assert_eq!(per_type_used(&runner), expected);
    }
}

/// With no land to sacrifice, Broodship's announcement is not on offer, and
/// Muldrotha's is (it auto-routes, being the only one castable).
#[test]
fn broodship_without_a_land_is_not_announced() {
    let (runner, guardian) = guardian_board(true, true, false);
    let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
    let broodship = object_named(&runner, "Exploration Broodship");
    let options =
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P0, guardian);
    assert!(option_index(&options, Method::Blitz, broodship, None).is_none());
    assert!(option_index(&options, Method::Blitz, muldrotha, None).is_some());
}

/// CR 601.2a: the announced permission is kept when its source leaves during
/// payment. Announce Broodship beside Muldrotha, then sacrifice Broodship to
/// Krark-Clan Ironworks for mana mid-payment: Broodship's slot is the one
/// spent, Muldrotha's stays free.
#[test]
fn announced_broodship_is_kept_when_it_leaves_mid_payment() {
    let (mut runner, guardian) = guardian_board_with(true, true, true, true);
    let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
    let broodship = object_named(&runner, "Exploration Broodship");
    let land = object_named(&runner, "Forest");
    let ironworks = object_named(&runner, "Krark-Clan Ironworks");
    let options = menu(&mut runner, guardian, CastPaymentMode::Manual);
    let index =
        option_index(&options, Method::Blitz, broodship, None).expect("blitz via Broodship");
    runner
        .act(GameAction::ChooseCastingVariant { index })
        .expect("announcing Broodship is legal");
    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .expect("paying Broodship's land sacrifice is legal");
    runner
        .act(GameAction::ActivateAbility {
            source_id: ironworks,
            ability_index: 0,
        })
        .expect("Ironworks is activatable during payment");
    runner
        .act(GameAction::SelectCards {
            cards: vec![broodship],
        })
        .expect("sacrificing Broodship for mana is legal");
    runner
        .act(GameAction::PassPriority)
        .expect("committing the payment completes the cast");
    assert_eq!(runner.state().objects[&guardian].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&broodship].zone, Zone::Graveyard);
    assert!(once_used(&runner, broodship));
    assert!(
        !per_type_used(&runner).contains(&(muldrotha, CoreType::Creature)),
        "Muldrotha was not announced, so its slot stays free"
    );
}

fn satyr_board(with_muldrotha: bool, with_broodship: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if with_broodship {
        add_exploration_broodship(&mut scenario, BROODSHIP_STATIONED);
    }
    if with_muldrotha {
        add_permission_source(
            &mut scenario,
            "Muldrotha, the Gravetide",
            MULDROTHA,
            &["Elemental", "Avatar"],
        );
    }
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut builder = scenario.add_creature_to_graveyard(P0, "Boon Satyr", 4, 2);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
    });
    builder.with_subtypes(vec!["Satyr"]);
    builder.from_oracle_text_with_keywords(&["Flash", "Bestow"], BOON_SATYR);
    let satyr = builder.id();
    let mut runner = scenario.build();
    add_core_type(&mut runner, satyr, CoreType::Enchantment);
    engine::game::layers::flush_layers(runner.state_mut());
    fill_mana(&mut runner, ManaType::Green);
    (runner, satyr)
}

/// The Bestow sibling: a bestowed Boon Satyr is an enchantment spell (CR
/// 702.103b). Announcing Broodship charges its land and spends its slot;
/// announcing Muldrotha spends its ENCHANTMENT slot and charges no land. Each
/// casts bestowed for {3}{G}{G}.
#[test]
fn satyr_bestow_announces_broodship_or_muldrotha() {
    for choose_broodship in [true, false] {
        let (mut runner, satyr) = satyr_board(true, true);
        let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
        let broodship = object_named(&runner, "Exploration Broodship");
        let land = object_named(&runner, "Forest");
        let source = if choose_broodship {
            broodship
        } else {
            muldrotha
        };
        cast_announcing(&mut runner, satyr, Method::Bestow, source, None);
        assert!(runner.state().objects[&satyr].bestow_form.is_some());
        assert_eq!(runner.state().players[0].mana_pool.total(), 3);
        assert_eq!(
            runner.state().objects[&land].zone == Zone::Graveyard,
            choose_broodship
        );
        assert_eq!(once_used(&runner, broodship), choose_broodship);
        let expected = if choose_broodship {
            vec![]
        } else {
            vec![(muldrotha, CoreType::Enchantment)]
        };
        assert_eq!(per_type_used(&runner), expected);
    }
}

// --- Muldrotha's per-type slot is part of a rider's announcement ----------

/// CR 110.4 + CR 702.103b: under Encroaching Mycosynth a bestowed Boon Satyr is
/// an artifact AND an enchantment spell, so bestow through Muldrotha is offered
/// once per slot, and the chosen slot is the one spent.
#[test]
fn mycosynth_bestow_through_muldrotha_is_announced_per_slot() {
    for slot in [CoreType::Artifact, CoreType::Enchantment] {
        let mut scenario = GameScenario::new();
        let satyr = mycosynth_muldrotha_board(&mut scenario);
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
        let mut runner = scenario.build();
        add_core_type(&mut runner, satyr, CoreType::Enchantment);
        engine::game::layers::flush_layers(runner.state_mut());
        fill_mana(&mut runner, ManaType::Green);
        let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
        let options = menu(
            &mut GameRunner::from_state(runner.state().clone()),
            satyr,
            CastPaymentMode::Auto,
        );
        for offered in [CoreType::Artifact, CoreType::Enchantment] {
            assert!(
                option_index(&options, Method::Bestow, muldrotha, Some(offered)).is_some(),
                "bestow via Muldrotha ({offered:?}) is offered, got {options:?}"
            );
        }
        assert!(
            option_index(
                &options,
                Method::Bestow,
                muldrotha,
                Some(CoreType::Creature)
            )
            .is_none(),
            "a bestowed Aura is not a creature spell"
        );
        cast_announcing(&mut runner, satyr, Method::Bestow, muldrotha, Some(slot));
        assert_eq!(per_type_used(&runner), vec![(muldrotha, slot)]);
    }
}

/// A forged Mycosynth bestow option with no slot, or with a slot the bestowed
/// form lacks (Creature), is refused before anything is paid.
#[test]
fn a_forged_bestow_slot_is_refused_before_payment() {
    for forged in [None, Some(CoreType::Creature)] {
        let mut scenario = GameScenario::new();
        let satyr = mycosynth_muldrotha_board(&mut scenario);
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
        let mut runner = scenario.build();
        add_core_type(&mut runner, satyr, CoreType::Enchantment);
        engine::game::layers::flush_layers(runner.state_mut());
        fill_mana(&mut runner, ManaType::Green);
        let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
        let mut options = menu(&mut runner, satyr, CastPaymentMode::Auto);
        let index = option_index(
            &options,
            Method::Bestow,
            muldrotha,
            Some(CoreType::Artifact),
        )
        .expect("the artifact slot is offered");
        let mut option = options[index].clone();
        option.authority.as_mut().unwrap().announcement.slot_type = forged;
        options.push(option);
        let forged_index = options.len() - 1;
        restore_menu(&mut runner, satyr, options);
        assert!(
            runner
                .act(GameAction::ChooseCastingVariant {
                    index: forged_index
                })
                .is_err(),
            "a forged slot {forged:?} is refused"
        );
        assert_eq!(runner.state().objects[&satyr].zone, Zone::Graveyard);
        assert_eq!(runner.state().players[0].mana_pool.total(), 8);
        assert!(per_type_used(&runner).is_empty());
    }
}

fn restore_menu(runner: &mut GameRunner, id: ObjectId, options: Vec<CastingVariantChoiceOption>) {
    let card_id = runner.state().objects[&id].card_id;
    runner.state_mut().waiting_for = WaitingFor::CastingVariantChoice {
        player: P0,
        object_id: id,
        card_id,
        payment_mode: CastPaymentMode::Auto,
        options,
    };
}

// --- The printed cost ------------------------------------------------------

fn bears_board(with_muldrotha: bool, with_lurrus: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if with_muldrotha {
        add_permission_source(
            &mut scenario,
            "Muldrotha, the Gravetide",
            MULDROTHA,
            &["Elemental", "Avatar"],
        );
    }
    if with_lurrus {
        add_permission_source(
            &mut scenario,
            "Lurrus of the Dream-Den",
            LURRUS,
            &["Cat", "Nightmare"],
        );
    }
    let bears = add_bears_to_graveyard(&mut scenario, "Grizzly Bears");
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 2);
    (runner, bears)
}

/// CR 601.2a + CR 601.2b: the printed cost is announced too. Muldrotha and
/// Lurrus both admit Grizzly Bears's printed {1}{G}: announcing Lurrus spends
/// Lurrus's once-per-turn slot, announcing Muldrotha its creature slot.
#[test]
fn printed_bears_announce_lurrus_or_muldrotha() {
    for choose_lurrus in [true, false] {
        let (mut runner, bears) = bears_board(true, true);
        let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
        let lurrus = object_named(&runner, "Lurrus of the Dream-Den");
        let source = if choose_lurrus { lurrus } else { muldrotha };
        cast_announcing(&mut runner, bears, Method::Printed, source, None);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(once_used(&runner, lurrus), choose_lurrus);
        let expected = if choose_lurrus {
            vec![]
        } else {
            vec![(muldrotha, CoreType::Creature)]
        };
        assert_eq!(per_type_used(&runner), expected);
    }
}

/// An unlimited permission beside Muldrotha is no longer taken for the player
/// as "dominant": both printed announcements are offered.
#[test]
fn printed_bears_offer_an_unlimited_permission_beside_muldrotha() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let open = add_permission_host(
        &mut scenario,
        "Open Permission",
        creature_permission(CastFrequency::Unlimited, None, vec![]),
    );
    let bears = add_bears_to_graveyard(&mut scenario, "Grizzly Bears");
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 2);
    let options = menu(
        &mut GameRunner::from_state(runner.state().clone()),
        bears,
        CastPaymentMode::Auto,
    );
    assert!(option_index(&options, Method::Printed, muldrotha, None).is_some());
    assert!(option_index(&options, Method::Printed, open, None).is_some());
    cast_announcing(&mut runner, bears, Method::Printed, muldrotha, None);
    assert_eq!(
        per_type_used(&runner),
        vec![(muldrotha, CoreType::Creature)]
    );
}

/// A single permission is used without a prompt.
#[test]
fn printed_bears_under_one_permission_need_no_announcement() {
    for (muldrotha, lurrus) in [(true, false), (false, true)] {
        let (mut runner, bears) = bears_board(muldrotha, lurrus);
        let waiting = cast_from_graveyard(&mut runner, bears).expect("the cast is legal");
        assert!(
            !matches!(waiting, WaitingFor::CastingVariantChoice { .. }),
            "one permission: no menu, got {waiting:?}"
        );
        assert_eq!(runner.state().objects[&bears].zone, Zone::Stack);
    }
}

fn ornithopter_board(with_muldrotha: bool, with_lurrus: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if with_muldrotha {
        add_permission_source(
            &mut scenario,
            "Muldrotha, the Gravetide",
            MULDROTHA,
            &["Elemental", "Avatar"],
        );
    }
    if with_lurrus {
        add_permission_source(
            &mut scenario,
            "Lurrus of the Dream-Den",
            LURRUS,
            &["Cat", "Nightmare"],
        );
    }
    // Ornithopter: {0} Artifact Creature — Thopter, 0/2, flying.
    let ornithopter = scenario
        .add_creature_to_graveyard(P0, "Ornithopter", 0, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    add_core_type(&mut runner, ornithopter, CoreType::Artifact);
    engine::game::layers::flush_layers(runner.state_mut());
    (runner, ornithopter)
}

/// CR 110.4: Muldrotha alone and an artifact creature card: the printed cast
/// asks which permanent type it is cast as, the prompt names Muldrotha's
/// permission, and the chosen slot is the one spent.
#[test]
fn sole_muldrotha_asks_for_the_slot_and_spends_it() {
    for slot in [CoreType::Artifact, CoreType::Creature] {
        let (mut runner, ornithopter) = ornithopter_board(true, false);
        let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
        let waiting = cast_from_graveyard(&mut runner, ornithopter).expect("the cast starts");
        match &waiting {
            WaitingFor::ChoosePermanentTypeSlot {
                source,
                permission,
                available_slots,
                ..
            } => {
                assert_eq!(*source, muldrotha);
                assert_eq!(
                    permission.as_ref().map(|p| p.permission.source),
                    Some(muldrotha),
                    "the prompt carries the announced permission"
                );
                assert!(available_slots.contains(&CoreType::Artifact));
                assert!(available_slots.contains(&CoreType::Creature));
            }
            other => panic!("expected the slot prompt, got {other:?}"),
        }
        runner
            .act(GameAction::ChoosePermanentTypeSlot { slot })
            .expect("choosing an offered slot is legal");
        assert_eq!(runner.state().objects[&ornithopter].zone, Zone::Stack);
        assert_eq!(per_type_used(&runner), vec![(muldrotha, slot)]);
    }
}

/// Muldrotha and Lurrus with Ornithopter (mana value 0, so Lurrus admits it):
/// announcing Muldrotha then asks for the slot, and only that slot is spent;
/// announcing Lurrus asks nothing more, spends Lurrus's slot and none of
/// Muldrotha's.
#[test]
fn ornithopter_announces_muldrotha_then_a_slot_or_lurrus() {
    let (mut runner, ornithopter) = ornithopter_board(true, true);
    let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
    let lurrus = object_named(&runner, "Lurrus of the Dream-Den");
    let options = menu(&mut runner, ornithopter, CastPaymentMode::Auto);
    let index = option_index(&options, Method::Printed, muldrotha, None).expect("via Muldrotha");
    assert_eq!(
        announcement(&options[index]).and_then(|a| a.slot_type),
        None,
        "the printed Muldrotha option asks for its slot after the announcement"
    );
    let waiting = runner
        .act(GameAction::ChooseCastingVariant { index })
        .expect("announcing Muldrotha is legal")
        .waiting_for;
    assert!(
        matches!(
            &waiting,
            WaitingFor::ChoosePermanentTypeSlot { permission: Some(p), .. }
                if p.permission.source == muldrotha
        ),
        "announcing Muldrotha asks for the slot, got {waiting:?}"
    );
    runner
        .act(GameAction::ChoosePermanentTypeSlot {
            slot: CoreType::Artifact,
        })
        .expect("choosing the artifact slot is legal");
    assert_eq!(runner.state().objects[&ornithopter].zone, Zone::Stack);
    assert_eq!(
        per_type_used(&runner),
        vec![(muldrotha, CoreType::Artifact)]
    );
    assert!(!once_used(&runner, lurrus));

    let (mut runner, ornithopter) = ornithopter_board(true, true);
    let options = menu(&mut runner, ornithopter, CastPaymentMode::Auto);
    let index = option_index(&options, Method::Printed, lurrus, None).expect("via Lurrus");
    let waiting = runner
        .act(GameAction::ChooseCastingVariant { index })
        .expect("announcing Lurrus is legal")
        .waiting_for;
    assert!(
        !matches!(waiting, WaitingFor::ChoosePermanentTypeSlot { .. }),
        "Lurrus has no per-type slot to ask for, got {waiting:?}"
    );
    assert_eq!(runner.state().objects[&ornithopter].zone, Zone::Stack);
    assert!(once_used(&runner, lurrus));
    assert!(per_type_used(&runner).is_empty());
}

/// CR 110.4 + CR 601.2a: a slot answered after it was spent is refused, before
/// anything is paid, even though the saved prompt still lists it.
#[test]
fn a_stale_slot_answer_is_refused_before_payment() {
    let (mut runner, ornithopter) = ornithopter_board(true, false);
    let muldrotha = object_named(&runner, "Muldrotha, the Gravetide");
    let waiting = cast_from_graveyard(&mut runner, ornithopter).expect("the cast starts");
    assert!(matches!(
        waiting,
        WaitingFor::ChoosePermanentTypeSlot { .. }
    ));
    runner
        .state_mut()
        .graveyard_cast_permissions_used_per_type
        .insert((muldrotha, CoreType::Artifact));
    assert!(runner
        .act(GameAction::ChoosePermanentTypeSlot {
            slot: CoreType::Artifact,
        })
        .is_err());
    assert_eq!(runner.state().objects[&ornithopter].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
}

// --- Stale, forged and legacy menu answers ---------------------------------

/// A menu answer is bound to the grant it names and to that grant's definition
/// (its digest): a forged grant, a changed digest, or an old menu option with
/// no announcement is refused with nothing paid.
#[test]
fn forged_stale_and_legacy_announcements_are_refused() {
    type Forge = fn(&mut CastingVariantChoiceOption);
    let forgeries: [(&str, Forge); 3] = [
        ("forged grant", |option| {
            option
                .authority
                .as_mut()
                .unwrap()
                .announcement
                .permission
                .source = ObjectId(9999);
        }),
        ("changed digest", |option| {
            option.authority.as_mut().unwrap().announcement.grant_digest =
                GrantDigest("0000000000000000".to_string());
        }),
        ("legacy option", |option| {
            option.authority = None;
        }),
    ];
    for (what, forge) in forgeries {
        let (mut runner, bears) = bears_board(true, true);
        let lurrus = object_named(&runner, "Lurrus of the Dream-Den");
        let mut options = menu(&mut runner, bears, CastPaymentMode::Auto);
        let index = option_index(&options, Method::Printed, lurrus, None).expect("via Lurrus");
        let mut option = options[index].clone();
        forge(&mut option);
        options.push(option);
        let forged = options.len() - 1;
        restore_menu(&mut runner, bears, options);
        assert!(
            runner
                .act(GameAction::ChooseCastingVariant { index: forged })
                .is_err(),
            "{what} is refused"
        );
        assert_eq!(
            runner.state().objects[&bears].zone,
            Zone::Graveyard,
            "{what}"
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), 2, "{what}");
        assert!(!once_used(&runner, lurrus), "{what}");
    }
}

/// The announcement survives a JSON round trip, including a digest above 2^53
/// (it travels as a string, so JavaScript peers keep every bit), and the
/// round-tripped menu is still answerable.
#[test]
fn an_announced_menu_survives_json() {
    let (mut runner, bears) = bears_board(true, true);
    let lurrus = object_named(&runner, "Lurrus of the Dream-Den");
    let options = menu(&mut runner, bears, CastPaymentMode::Auto);
    let digest = &announcement(&options[0]).unwrap().grant_digest.0;
    assert_eq!(digest.len(), 16);
    let big = GrantDigest(format!("{:016x}", (1u64 << 60) + 1));
    let json = serde_json::to_string(&big).unwrap();
    assert_eq!(
        json,
        format!("\"{}\"", big.0),
        "the digest is a JSON string"
    );
    let state_json = serde_json::to_string(runner.state()).unwrap();
    let restored: engine::types::game_state::GameState = serde_json::from_str(&state_json).unwrap();
    let mut runner = GameRunner::from_state(restored);
    let WaitingFor::CastingVariantChoice { options, .. } = runner.state().waiting_for.clone()
    else {
        panic!("the menu is restored");
    };
    let index = option_index(&options, Method::Printed, lurrus, None).expect("via Lurrus");
    runner
        .act(GameAction::ChooseCastingVariant { index })
        .expect("the restored menu is answerable");
    assert!(once_used(&runner, lurrus));
}

// --- Two bounded grants on one source: unsupported, fails closed ----------

/// A Caldaia Guardian in the graveyard beside `host`, a creature carrying
/// `statics`, and optionally `granter`, a noncreature permanent carrying a
/// static that grants creatures a permission.
fn shared_slot_board(
    statics: Vec<engine::types::ability::StaticDefinition>,
    granter: Option<engine::types::ability::StaticDefinition>,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder = scenario.add_creature(P0, "Permission Host", 1, 1);
    for definition in statics {
        builder.with_static_definition(definition);
    }
    let host = builder.id();
    if let Some(grant) = granter {
        scenario
            .add_artifact_from_oracle(P0, "Permission Granter", "")
            .with_static_definition(grant);
    }
    let guardian = scenario
        .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(caldaia_blitz())
        .id();
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    fill_mana(&mut runner, ManaType::Green);
    (runner, host, guardian)
}

fn once_per_turn(
    required: Option<engine::types::keywords::KeywordKind>,
) -> engine::types::ability::StaticDefinition {
    creature_permission(CastFrequency::OncePerTurn, required, vec![])
}

/// The open bounded grant B, as the menu offers it when it is the host's
/// grant at `index` and the only bounded one there.
fn announced_open_grant(index_filler: bool) -> CastingVariantChoiceOption {
    let mut statics = Vec::new();
    if index_filler {
        statics.push(engine::types::ability::StaticDefinition::continuous());
    }
    statics.push(once_per_turn(None));
    let (runner, host, guardian) = shared_slot_board(statics, None);
    let options =
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P0, guardian);
    options[option_index(&options, Method::Printed, host, None)
        .expect("a lone bounded grant is offered (reach control)")]
    .clone()
}

fn assert_shared_slot_refused(runner: &mut GameRunner, host: ObjectId, guardian: ObjectId) {
    let options =
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P0, guardian);
    assert!(
        options
            .iter()
            .all(|option| announcement(option).is_none_or(|a| a.permission.source != host)),
        "no grant on a source with two bounded grants is offered, got {options:?}"
    );
    assert!(
        offered_cast(runner, guardian).is_none(),
        "and no cast through them is a legal action"
    );
}

/// CR 601.2a: one source carrying two once-per-turn graveyard permissions (A
/// requires blitz, B is open) shares one per-turn ledger slot between them,
/// which the engine can't charge separately. Neither is offered, and a menu
/// answer naming B is refused as unsupported. (A lone bounded grant is offered:
/// `announced_open_grant`.)
#[test]
fn two_bounded_grants_on_one_source_offer_neither() {
    let open = announced_open_grant(true);
    let (mut runner, host, guardian) = shared_slot_board(
        vec![
            once_per_turn(Some(engine::types::keywords::KeywordKind::Blitz)),
            once_per_turn(None),
        ],
        None,
    );
    assert_shared_slot_refused(&mut runner, host, guardian);
    let mut option = open;
    option
        .authority
        .as_mut()
        .unwrap()
        .announcement
        .permission
        .source = host;
    restore_menu(&mut runner, guardian, vec![option]);
    let err = runner
        .act(GameAction::ChooseCastingVariant { index: 0 })
        .expect_err("a grant sharing its source's slot is refused");
    assert!(
        format!("{err:?}").contains("share one source"),
        "refused as the unsupported shared slot, got {err:?}"
    );
    assert_eq!(runner.state().objects[&guardian].zone, Zone::Graveyard);
}

/// The same when the second bounded grant arrives during the game: a
/// noncreature permanent grants creatures a once-per-turn permission, and the
/// host already prints one.
#[test]
fn a_granted_second_bounded_grant_offers_neither() {
    let grant = engine::types::ability::StaticDefinition::continuous()
        .affected(engine::types::ability::TargetFilter::Typed(
            engine::types::ability::TypedFilter::creature(),
        ))
        .modifications(vec![
            engine::types::ability::ContinuousModification::GrantStaticAbility {
                definition: Box::new(once_per_turn(Some(
                    engine::types::keywords::KeywordKind::Blitz,
                ))),
            },
        ]);
    let (mut runner, host, guardian) = shared_slot_board(vec![once_per_turn(None)], Some(grant));
    assert!(
        runner.state().objects[&host].static_definitions.len() > 1,
        "reach: the host received the granted permission, statics: {:?}",
        runner.state().objects[&host].static_definitions
    );
    assert_shared_slot_refused(&mut runner, host, guardian);
}

// --- Affordability without a single exact cost ------------------------------

fn casts(runner: &GameRunner, id: ObjectId) -> bool {
    engine::ai_support::legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

/// CR 601.2a + CR 601.2f: with two permissions there is no exact cost until
/// one is announced (`effective_spell_cost` keeps its exact contract and says
/// `None`), yet the cast is payable, so the existential check admits it and
/// legal actions offer it; dispatch then asks for the announcement.
#[test]
fn a_two_permission_cast_is_payable_by_some_option() {
    let (mut runner, bears) = bears_board(true, true);
    assert_eq!(
        engine::game::casting::effective_spell_cost(runner.state(), P0, bears),
        None,
        "no single exact cost before the announcement"
    );
    assert!(
        engine::game::casting::graveyard_cast_payable_by_some_option(
            runner.state(),
            P0,
            bears,
            None
        )
    );
    assert!(casts(&runner, bears));
    menu(&mut runner, bears, CastPaymentMode::Auto);
}

/// The zero-option negative: without mana no option is payable, and no cast
/// is offered.
#[test]
fn a_two_permission_cast_with_no_payable_option_is_not_offered() {
    let (mut runner, bears) = bears_board(true, true);
    runner.state_mut().players[0].mana_pool = Default::default();
    assert!(
        !engine::game::casting::graveyard_cast_payable_by_some_option(
            runner.state(),
            P0,
            bears,
            None
        )
    );
    assert!(!casts(&runner, bears));
}
