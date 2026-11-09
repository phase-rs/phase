//! Full output from one any-one-color activation through public payment.
//! CR 106.3 + CR 106.4: produce the instructed amount and retain unspent units.
//! CR 107.4b: remaining units may pay generic costs; CR 106.6 preserves restrictions.

use engine::ai_support::{legal_actions, legal_actions_full};
use engine::game::casting::activated_ability_definitions;
use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::mana_payment::{
    can_pay_for_spell, compute_phyrexian_shards, pay_cost_with_demand_and_choices, pay_from_pool,
};
use engine::game::mana_sources::{
    activatable_land_mana_options, activatable_mana_options, activatable_mana_source_selections,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, ActivationManaPaymentRestriction, ActivationRestriction, ChosenAttribute, Effect,
    ManaProduction, ManaSpendPermission,
};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, ManaChoice, ManaChoicePrompt, PayCostKind, ShardChoice, ShardOptions,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{
    parse_mtgjson_mana_cost, ActivationManaColorConstraint, CostPermissionContext,
    LifePaymentColors, ManaColor, ManaCost, ManaPipId, ManaPool, ManaRestriction, ManaSourceOutput,
    ManaSourceQuantity, ManaSupertype, ManaType, ManaUnit, PaymentContext, SpellMeta,
};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LOTUS: &str = "{T}: Add three mana of any one color.";
const LUTE: &str = "Lands you control have \"{T}: Add two mana of any one color. Spend this mana only to cast instant and sorcery spells.\"\n{T}: Draw a card. Activate only if you have seven or more cards in your hand.";
const GOLDSPAN: &str = "Flying, haste\nWhenever this creature attacks or becomes the target of a spell, create a Treasure token.\nTreasures you control have \"{T}, Sacrifice this artifact: Add two mana of any one color.\"";
const ASTRAL: &str = "This artifact enters with X charge counters on it.\n{T}: Choose a color. Add one mana of that color for each charge counter on this artifact.";
const NYX: &str = "Nyx Lotus enters tapped.\n{T}: Choose a color. Add an amount of mana of that color equal to your devotion to that color. (Your devotion to a color is the number of mana symbols of that color in the mana costs of permanents you control.)";
const BAKU: &str = "Whenever you cast a Spirit or Arcane spell, you may put a ki counter on this creature.\n{1}, Remove X ki counters from this creature: Add X mana of any one color.";
const CHARM: &str = "Choose one —\n• Counter target spell.\n• Target player draws two cards.\n• Gain control of target nonland permanent with mana value 1 or less.";
const MIND_STONE: &str = "{T}: Add {C}.\n{1}, {T}, Sacrifice this artifact: Draw a card.";
const KING: &str = "({2/W} can be paid with any two mana or with {W}. This card's mana value is 10.)\nOther Scarecrow creatures you control get +1/+1.\nWhenever another Scarecrow you control enters, destroy target permanent.";
const SPATIAL: &str =
    "({C} represents colorless mana.)\nTarget creature gets +3/-3 until end of turn.";
const LOTUS_FIELD: &str = "Hexproof\nThis land enters tapped.\nWhen this land enters, sacrifice two lands.\n{T}: Add three mana of any one color.";
const WILD_GROWTH: &str = "Enchant land\nWhenever enchanted land is tapped for mana, its controller adds an additional {G}.";
const OVERGROWTH: &str = "Enchant land\nWhenever enchanted land is tapped for mana, its controller adds an additional {G}{G}.";
const FERTILE_GROUND: &str = "Enchant land\nWhenever enchanted land is tapped for mana, its controller adds an additional one mana of any color.";
const OWL: &str = "Flying\nRimefeather Owl's power and toughness are each equal to the number of snow permanents on the battlefield.\n{1}{S}: Put an ice counter on target permanent.\nPermanents with ice counters on them are snow.";
const WOWZER: &str = "Whenever Wowzer, the Aspirational attacks, if you have an {E}, control a Blood, a Clue, a Food, a Map, a Powerstone, and a Treasure, are the monarch, and have the city's blessing and the initiative, you win the game.";
const RAPTOR: &str = "Flying\n{S}{S}: This creature gains shroud until end of turn. ({S} can be paid with one mana from a snow source. A creature with shroud can't be the target of spells or abilities.)";
const NYKTHOS: &str = "{T}: Add {C}.\n{2}, {T}: Choose a color. Add an amount of mana of that color equal to your devotion to that color. (Your devotion to a color is the number of mana symbols of that color in the mana costs of permanents you control.)";
const SPIRAL: &str = "Draw a card. You may put a land card from your hand onto the battlefield.";
const QUARRY: &str = "{T}: Add {C}.\n{T}, Sacrifice a creature: Add one mana of any color.\n{X}{2}, {T}, Sacrifice a Desert: Exile target creature card with mana value X from your graveyard. Create a token that's a copy of it, except it's a 4/4 black Zombie. Activate only as a sorcery.";
const BASAL_THRULL: &str = "{T}, Sacrifice this creature: Add {B}{B}.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const LEYLINE_VOID: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";

fn pay_glint_eye_with_an_enchanted_land(land_name: &str, land_text: &str, count: usize) {
    for (aura_name, aura_text) in [
        ("Wild Growth", "Enchant land\nWhenever enchanted land is tapped for mana, its controller adds an additional {G}."),
        ("Fertile Ground", "Enchant land\nWhenever enchanted land is tapped for mana, its controller adds an additional one mana of any color."),
    ] {
        let mut s = board();
        let land = s.add_land_from_oracle(P0, land_name, land_text).id();
        let wastes = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
        let aura = s.add_enchantment_from_oracle(P0, aura_name, aura_text)
            .with_subtypes(vec!["Aura"]).id();
        let signet = s.add_artifact_from_oracle(P0, "Dimir Signet", "{1}, {T}: Add {U}{B}.").id();
        let nephilim = s.add_creature_to_hand_from_oracle(P0, "Glint-Eye Nephilim", 2, 2,
            "Whenever this creature deals combat damage to a player, draw that many cards.\n{1}, Discard a card: This creature gets +1/+1 until end of turn.")
            .with_mana_cost(parse_mtgjson_mana_cost("{U}{B}{R}{G}")).id();
        let mut r = s.build();
        // These are established permanents after any enters-tapped/sacrifice
        // work; the public payment starts with an untapped enchanted land.
        r.state_mut().objects.get_mut(&land).unwrap().tapped = false;
        engine::game::effects::attach::attach_to(r.state_mut(), aura, land);
        evaluate_layers(r.state_mut());
        // Manual choices intentionally defer the color and omit concrete
        // combinations. The auto-pay input retains the land and Aura units.
        let rows = activatable_land_mana_options(r.state(), land, P0);
        assert!(rows.iter().any(|row| {
            row.object_id == land
                && row.atomic_combination.as_ref().is_some_and(|combo| combo.len() == count + 1)
                && row.taps_for_mana_overrides.len() == 1
        }), "the printed land and Aura expose their separate full outputs");
        let (_, _, grouped) = legal_actions_full(r.state());
        let float = grouped[&wastes].iter().find(|action| matches!(action,
            GameAction::TapLandForMana { selection } if selection.mana_type == ManaType::Colorless))
            .expect("Wastes offers its printed mana ability").clone();
        r.act(float).unwrap();
        assert_eq!(r.state().players[0].mana_pool.count_color(ManaType::Colorless), 1);
        r.cast(nephilim).resolve().assert_zone(&[nephilim], Zone::Battlefield);
        assert!([land, wastes, signet].iter().all(|id| r.state().objects[id].tapped));
        assert_eq!(r.state().objects[&aura].attached_to, Some(AttachTarget::Object(land)));
        assert!(r.state().players[0].mana_pool.units().all(|unit| unit.source_id == land));
        assert_complete(&r, count - 1);
    }
}

/// CR 106.3 + CR 118.10: floating C funds the Signet; the land's R and
/// its Aura's G remain separate outputs for the printed four-color spell.
#[test]
fn floating_mana_keeps_a_city_of_brass_aura_bonus() {
    pay_glint_eye_with_an_enchanted_land("City of Brass",
        "Whenever this land becomes tapped, it deals 1 damage to you.\n{T}: Add one mana of any color.", 1);
}

/// CR 106.3: scalar planning keeps the Aura bonus while replay still adds
/// all three mana from Lotus Field; two units remain after the real cast.
#[test]
fn floating_mana_keeps_a_counted_lands_aura_bonus() {
    pay_glint_eye_with_an_enchanted_land("Lotus Field",
        "Hexproof\nThis land enters tapped.\nWhen this land enters, sacrifice two lands.\n{T}: Add three mana of any one color.", 3);
}

#[test]
fn snow_hybrid_payment_preserves_a_quality_eligible_alternative() {
    // Supplemental combined costs: no printing combines S and these hybrid
    // symbols. Arcum's Astrolabe supplies S, Judge's Familiar W/U, and Ulalek
    // C/U. CR 107.4e/h: ordinary mana pays the hybrid; Snow remains for S.
    for (symbol, a, b) in [
        ("{W/U}", ManaType::White, ManaType::Blue),
        ("{C/U}", ManaType::Colorless, ManaType::Blue),
    ] {
        for (snow_color, plain_color) in [(a, b), (b, a)] {
            for snow_first in [true, false] {
                for phyrexian in [false, true] {
                    let text = if snow_first {
                        format!("{{S}}{symbol}")
                    } else {
                        format!("{symbol}{{S}}")
                    };
                    let text = if phyrexian {
                        format!("{text}{{R/P}}")
                    } else {
                        text
                    };
                    let cost = parse_mtgjson_mana_cost(&text);
                    let mut units = vec![
                        hybrid_reserve_unit(snow_color, 1, true, false),
                        snow_regression_unit(plain_color, 2),
                    ];
                    if phyrexian {
                        units.push(snow_regression_unit(ManaType::Red, 3));
                    }
                    let mut pool = ManaPool::from_units(units);
                    assert!(
                        can_pay_for_spell(&pool, &cost, None, CostPermissionContext::default()),
                        "{text}"
                    );
                    if phyrexian {
                        let preview = compute_phyrexian_shards(
                            &pool,
                            &cost,
                            None,
                            CostPermissionContext {
                                max_life: 1,
                                ..Default::default()
                            },
                        );
                        assert_eq!(preview.len(), 1);
                        assert_eq!(preview[0].shard_index, 2);
                        assert_eq!(preview[0].options, ShardOptions::ManaOrLife);
                    }
                    let choices = [ShardChoice::PayMana];
                    let (spent, life) = pay_cost_with_demand_and_choices(
                        &mut pool,
                        &cost,
                        None,
                        None,
                        None,
                        phyrexian.then_some(choices.as_slice()),
                        LifePaymentColors::EMPTY,
                        &[],
                    )
                    .unwrap();
                    assert_eq!(spent.len(), if phyrexian { 3 } else { 2 });
                    assert!(life.is_empty());
                    assert!(pool.is_empty());
                }
            }
        }
    }
    // CR 106.6: a restricted ordinary alternative cannot rescue the hybrid.
    let cost = parse_mtgjson_mana_cost("{S}{W/U}");
    let mut pool = ManaPool::from_units(vec![
        hybrid_reserve_unit(ManaType::White, 1, true, false),
        ManaUnit {
            restrictions: vec![ManaRestriction::OnlyForActivation],
            ..snow_regression_unit(ManaType::Blue, 2)
        },
    ]);
    let original = pool.clone();
    assert!(!can_pay_for_spell(
        &pool,
        &cost,
        Some(&PaymentContext::Effect),
        CostPermissionContext::default()
    ));
    assert!(pay_cost_with_demand_and_choices(
        &mut pool,
        &cost,
        None,
        Some(&PaymentContext::Effect),
        None,
        None,
        LifePaymentColors::EMPTY,
        &[]
    )
    .is_err());
    assert_eq!(pool, original);

    // CR 107.4e + CR 118.3a: a pin prefers that unit, not its assignment to
    // a particular shard. Snow W pays S while ordinary U pays W/U.
    let mut pinned = ManaPool::from_units(vec![
        hybrid_reserve_unit(ManaType::White, 1, true, false),
        snow_regression_unit(ManaType::Blue, 2),
    ]);
    let (spent, life) = pay_cost_with_demand_and_choices(
        &mut pinned,
        &cost,
        None,
        None,
        None,
        None,
        LifePaymentColors::EMPTY,
        &[ManaPipId(1)],
    )
    .expect("the pinned Snow unit remains spendable on the Snow shard");
    assert!(life.is_empty());
    assert_eq!(spent.len(), 2);
    assert!(spent.iter().any(|unit| unit.pip_id == ManaPipId(1)));
    assert_eq!(pinned.total(), 0);

    // The remaining U/P can use life. It must not force Snow W into W/U;
    // mana-only residual planning's conservative future demand is not a
    // requirement of the completed payment (CR 107.4f).
    let cost = parse_mtgjson_mana_cost("{S}{W/U}{U/P}");
    let mut pool = ManaPool::from_units(vec![
        hybrid_reserve_unit(ManaType::White, 1, true, false),
        snow_regression_unit(ManaType::Blue, 2),
    ]);
    let permission = CostPermissionContext {
        max_life: 1,
        ..Default::default()
    };
    assert!(can_pay_for_spell(&pool, &cost, None, permission));
    let preview = compute_phyrexian_shards(&pool, &cost, None, permission);
    assert_eq!(preview.len(), 1);
    assert_eq!(preview[0].shard_index, 2);
    assert_eq!(preview[0].options, ShardOptions::LifeOnly);
    let (spent, life) = pay_cost_with_demand_and_choices(
        &mut pool,
        &cost,
        None,
        None,
        None,
        Some(&[ShardChoice::PayLife]),
        LifePaymentColors::EMPTY,
        &[],
    )
    .unwrap();
    assert_eq!(spent.len(), 2);
    assert_eq!(life.len(), 1);
    assert!(pool.is_empty());
}

#[test]
fn fully_floated_snow_hybrid_cast_keeps_the_only_snow_unit() {
    // Supplemental costs: no printing combines Snow and these hybrids.
    // Astrolabe supplies S; Opt supplies U, Judge's Familiar W/U, Ulalek C/U.
    // CR 107.4e/h: the ordinary Island unit pays the hybrid, preserving
    // the Snow source's unit for S. Every source is exhausted before casting.
    for (symbol, snow_name, snow_text, snow_color) in [
        ("{W/U}", "Snow-Covered Plains", "({T}: Add {W}.)", ManaType::White),
        ("{C/U}", "Faceless Haven", "{T}: Add {C}.\n{S}{S}{S}: This land becomes a 4/3 creature with vigilance and all creature types until end of turn. It's still a land. ({S} can be paid with one mana from a snow source.)", ManaType::Colorless),
        ("{U}", "Snow-Covered Island", "({T}: Add {U}.)", ManaType::Blue),
        ("{W/U}", "Snow-Covered Island", "({T}: Add {U}.)", ManaType::Blue),
    ] {
        for (snow_first, pin_snow, any_color) in [
            (true, false, false),
            (false, false, false),
            (true, true, false),
            (false, true, false),
            (true, true, true),
            (false, true, true),
        ] {
            let mut s = board();
            if any_color {
                s.add_artifact_from_oracle(P0, "Mycosynth Lattice",
                    "All permanents are artifacts in addition to their other types.\nAll cards that aren't on the battlefield, spells, and permanents are colorless.\nPlayers may spend mana as though it were mana of any color.")
                    .with_mana_cost(parse_mtgjson_mana_cost("{6}"));
            }
            let snow = if snow_color == ManaType::White {
                s.add_land_from_oracle(P0, snow_name, snow_text).as_snow()
                    .with_subtypes(vec!["Plains"]).id()
            } else if snow_color == ManaType::Blue {
                s.add_land_from_oracle(P0, snow_name, snow_text).as_snow()
                    .with_subtypes(vec!["Island"]).id()
            } else {
                s.add_land_from_oracle(P0, snow_name, snow_text).as_snow().id()
            };
            let island = s.add_basic_land(P0, ManaColor::Blue);
            let cost = if snow_first { format!("{{S}}{symbol}") } else { format!("{symbol}{{S}}") };
            let astrolabe = s.add_artifact_to_hand_from_oracle(P0, "Arcum's Astrolabe",
                "({S} can be paid with one mana from a snow source.)\nWhen this artifact enters, draw a card.\n{1}, {T}: Add one mana of any color.")
                .as_snow().with_mana_cost(parse_mtgjson_mana_cost(&cost)).id();
            let mut r = s.build();
            if snow_color != ManaType::Colorless {
                finish_snow_forest(&mut r, snow);
            }
            evaluate_layers(r.state_mut());
            assert_eq!(engine::game::static_abilities::player_mana_spend_permission_for_spell_object(r.state(), P0, astrolabe),
                any_color.then_some(ManaSpendPermission::AnyColor));
            for (source, color) in [(snow, snow_color), (island, ManaType::Blue)] {
                let (_, _, grouped) = legal_actions_full(r.state());
                let action = grouped[&source].iter().find(|action| matches!(action,
                    GameAction::TapLandForMana { selection } if selection.mana_type == color))
                    .expect("the real land offers its chosen mana action").clone();
                r.act(action).unwrap();
            }
            assert!([snow, island].iter().all(|id| r.state().objects[id].tapped));
            assert!(activatable_mana_source_selections(r.state(), P0).is_empty());
            let pool = &r.state().players[0].mana_pool;
            assert_eq!(pool.total(), 2);
            assert_eq!(pool.units().filter(|unit| unit.source_id == snow
                && unit.color == snow_color && unit.supertype == Some(ManaSupertype::Snow)).count(), 1);
            assert_eq!(pool.units().filter(|unit| unit.source_id == island
                && unit.color == ManaType::Blue && unit.supertype.is_none()).count(), 1);
            if pin_snow {
                let pip_id = pool.units().find(|unit| unit.source_id == snow).unwrap().pip_id;
                r.act(GameAction::CastSpell {
                    object_id: astrolabe,
                    card_id: r.state().objects[&astrolabe].card_id,
                    targets: Vec::new(),
                    payment_mode: CastPaymentMode::Manual,
                }).unwrap();
                assert!(matches!(r.state().waiting_for, WaitingFor::ManaPayment { player: P0, .. }));
                r.act(GameAction::SpendPoolMana { pip_id }).unwrap();
                assert_eq!(r.state().pending_cast.as_ref().unwrap().pinned_pool_units, vec![pip_id]);
                r.act(GameAction::PassPriority).unwrap_or_else(|error| panic!(
                    "{symbol}, snow_first={snow_first}, lattice={any_color}: pinned Snow payment failed: {error:?}"));
                assert_eq!(r.state().objects[&astrolabe].zone, Zone::Stack);
                assert_eq!(r.state().objects[&astrolabe].mana_spent_to_cast_amount, 2);
                assert!(!r.state().players[0].mana_pool.units().any(|unit| unit.pip_id == pip_id));
                r.advance_until_stack_empty();
                assert_eq!(r.state().objects[&astrolabe].zone, Zone::Battlefield);
            } else {
                r.cast(astrolabe).resolve().assert_zone(&[astrolabe], Zone::Battlefield);
            }
            assert_complete(&r, 0);
        }
    }
}

#[test]
fn snow_hybrid_partial_pool_keeps_the_later_strict_color() {
    // Supplemental S + W/U + U cost on the real Astrolabe fixture; no
    // printing has this combined cost. The old partial path spends Snow W
    // on W/U, keeps U for strict U, then taps Haven for S. Switching to U
    // merely to reserve Snow would strand the later U requirement.
    let mut s = board();
    let plains = s
        .add_land_from_oracle(P0, "Snow-Covered Plains", "({T}: Add {W}.)")
        .as_snow()
        .with_subtypes(vec!["Plains"])
        .id();
    let island = s.add_basic_land(P0, ManaColor::Blue);
    let haven = s.add_land_from_oracle(P0, "Faceless Haven",
        "{T}: Add {C}.\n{S}{S}{S}: This land becomes a 4/3 creature with vigilance and all creature types until end of turn. It's still a land. ({S} can be paid with one mana from a snow source.)")
        .as_snow().id();
    let astrolabe = s.add_artifact_to_hand_from_oracle(P0, "Arcum's Astrolabe",
        "({S} can be paid with one mana from a snow source.)\nWhen this artifact enters, draw a card.\n{1}, {T}: Add one mana of any color.")
        .as_snow().with_mana_cost(parse_mtgjson_mana_cost("{W/U}{S}{U}")).id();
    let mut r = s.build();
    finish_snow_forest(&mut r, plains);
    evaluate_layers(r.state_mut());
    for (source, color) in [(plains, ManaType::White), (island, ManaType::Blue)] {
        let (_, _, grouped) = legal_actions_full(r.state());
        let action = grouped[&source]
            .iter()
            .find(|action| {
                matches!(action,
            GameAction::TapLandForMana { selection } if selection.mana_type == color)
            })
            .expect("the real basic land offers its mana action")
            .clone();
        r.act(action).unwrap();
    }
    assert!(!r.state().objects[&haven].tapped);
    assert_eq!(r.state().players[0].mana_pool.total(), 2);
    r.cast(astrolabe)
        .resolve()
        .assert_zone(&[astrolabe], Zone::Battlefield);
    assert!([plains, island, haven]
        .iter()
        .all(|id| r.state().objects[id].tapped));
    assert_complete(&r, 0);
}

#[test]
fn snow_hybrid_partial_pool_counts_surplus_blue_before_tapping_generic() {
    // Supplemental S + hybrid + U: no printing has this complete cost.
    // Astrolabe, Judge's Familiar and Ulalek supply the printed primitives;
    // Sphere of Resistance adds the real generic tax (CR 601.2f).
    // With two ordinary U units, one can pay the hybrid while preserving
    // the Snow unit. With only one usable U, keep it for the strict U and
    // pay S from the spare Haven. Lute's restricted U is not usable surplus
    // for this artifact (CR 106.6 + CR 107.4e/h).
    const HAVEN: &str = "{T}: Add {C}.\n{S}{S}{S}: This land becomes a 4/3 creature with vigilance and all creature types until end of turn. It's still a land. ({S} can be paid with one mana from a snow source.)";
    for (symbol, snow_name, snow_text, snow_color) in [
        (
            "{W/U}",
            "Snow-Covered Plains",
            "({T}: Add {W}.)",
            ManaType::White,
        ),
        ("{C/U}", "Faceless Haven", HAVEN, ManaType::Colorless),
    ] {
        for (blue_count, restricted_extra) in [(1, false), (2, false), (1, true)] {
            let mut s = board();
            let snow = if snow_color == ManaType::White {
                s.add_land_from_oracle(P0, snow_name, snow_text)
                    .as_snow()
                    .with_subtypes(vec!["Plains"])
                    .id()
            } else {
                s.add_land_from_oracle(P0, snow_name, snow_text)
                    .as_snow()
                    .id()
            };
            let islands: Vec<_> = (0..blue_count)
                .map(|_| s.add_basic_land(P0, ManaColor::Blue))
                .collect();
            let wastes = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
            let spare_snow = (blue_count == 1).then(|| {
                s.add_land_from_oracle(P0, "Faceless Haven", HAVEN)
                    .as_snow()
                    .id()
            });
            let restricted_land = if restricted_extra {
                s.add_artifact_from_oracle(P0, "Resonating Lute", LUTE);
                Some(s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id())
            } else {
                None
            };
            s.add_artifact_from_oracle(P0, "Sphere of Resistance", "Spells cost {1} more to cast.");
            let astrolabe = s.add_artifact_to_hand_from_oracle(P0, "Arcum's Astrolabe",
                "({S} can be paid with one mana from a snow source.)\nWhen this artifact enters, draw a card.\n{1}, {T}: Add one mana of any color.")
                .as_snow().with_mana_cost(parse_mtgjson_mana_cost(&format!("{{S}}{symbol}{{U}}"))).id();
            let mut r = s.build();
            if snow_color == ManaType::White {
                finish_snow_forest(&mut r, snow);
            }
            evaluate_layers(r.state_mut());
            for (source, color) in std::iter::once((snow, snow_color))
                .chain(islands.iter().map(|&id| (id, ManaType::Blue)))
            {
                let (_, _, grouped) = legal_actions_full(r.state());
                let action = grouped[&source]
                    .iter()
                    .find(|action| {
                        matches!(action,
                    GameAction::TapLandForMana { selection }
                        if selection.mana_type == color && selection.restrictions.is_empty())
                    })
                    .expect("the real land offers unrestricted printed mana")
                    .clone();
                r.act(action).unwrap();
            }
            if let Some(source) = restricted_land {
                let (_, _, grouped) = legal_actions_full(r.state());
                let action = grouped[&source].iter().find(|action| matches!(action,
                    GameAction::TapLandForMana { selection }
                        if selection.mana_type == ManaType::Blue
                            && !selection.restrictions.is_empty()
                            && selection.atomic_combination.as_ref().is_some_and(|units| units.len() == 2)))
                    .expect("Lute offers two restricted blue units on this land").clone();
                r.act(action).unwrap();
                assert_eq!(
                    r.state().players[0]
                        .mana_pool
                        .units()
                        .filter(|unit| unit.source_id == source && !unit.restrictions.is_empty())
                        .count(),
                    2
                );
            }
            assert!(r.state().objects[&snow].tapped);
            assert!(islands.iter().all(|id| r.state().objects[id].tapped));
            assert!(!r.state().objects[&wastes].tapped);
            assert!(spare_snow.is_none_or(|id| !r.state().objects[&id].tapped));
            assert_eq!(
                r.state().players[0].mana_pool.total(),
                1 + blue_count + if restricted_extra { 2 } else { 0 }
            );
            assert_eq!(
                r.state().players[0]
                    .mana_pool
                    .units()
                    .filter(|unit| unit.source_id == snow
                        && unit.supertype == Some(ManaSupertype::Snow))
                    .count(),
                1
            );
            assert!(activatable_mana_source_selections(r.state(), P0)
                .iter()
                .all(|selection| selection.source.object_id == wastes
                    || Some(selection.source.object_id) == spare_snow));
            eprintln!("partial hybrid {symbol}: ordinary blue={blue_count}, restricted extra={restricted_extra}");
            r.cast(astrolabe)
                .resolve()
                .assert_zone(&[astrolabe], Zone::Battlefield);
            assert!(r.state().objects[&wastes].tapped);
            assert!(spare_snow.is_none_or(|id| r.state().objects[&id].tapped));
            assert_complete(&r, if restricted_extra { 2 } else { 0 });
        }
    }
}

fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Island", "Island", "Island", "Island"]);
    }
    scenario
}

fn spell(s: &mut GameScenario, name: &str, instant: bool, text: &str, cost: &str) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, name, instant, text)
        .with_mana_cost(parse_mtgjson_mana_cost(cost))
        .id()
}

fn assert_complete(r: &GameRunner, pool: usize) {
    assert!(r.state().stack.is_empty());
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(r.state().players[0].mana_pool.total(), pool);
}

#[test]
fn toluz_keeps_a_payable_hybrid_suffix_with_ineligible_mana() {
    // CR 106.6 + CR 107.4e + CR 601.2h: W/U, U, U/B can use W/U/U.
    // The Lute's spell-only mana cannot pay for this creature. Eligible U=2
    // must not make the first hybrid strand the later strict U and U/B.
    const TOLUZ: &str = "When Toluz enters, it connives. (Draw a card, then discard a card. If you discarded a nonland card, put a +1/+1 counter on this creature.)\nWhenever you discard one or more cards, exile them from your graveyard.\nWhen Toluz dies, put the cards exiled with it into their owner's hand.";
    for (extra, krrik, pinned) in [
        (Some(ManaType::White), false, false),
        (Some(ManaType::Blue), false, false),
        (None, false, false),
        (Some(ManaType::White), true, false),
        (None, false, true),
        (Some(ManaType::Blue), false, true),
    ] {
        let mut s = board();
        if krrik {
            s.with_life(P0, 1);
            s.add_creature_from_oracle(P0, "K'rrik, Son of Yawgmoth", 2, 2,
                "({B/P} can be paid with either {B} or 2 life.)\nLifelink\nFor each {B} in a cost, you may pay 2 life rather than pay that mana.\nWhenever you cast a black spell, put a +1/+1 counter on K'rrik.")
                .as_legendary()
                .with_subtypes(vec!["Phyrexian", "Horror", "Minion"])
                .with_mana_cost(parse_mtgjson_mana_cost("{4}{B/P}{B/P}{B/P}"));
        }
        s.add_artifact_from_oracle(P0, "Resonating Lute", LUTE)
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{U}{R}"));
        let plains = s.add_basic_land(P0, ManaColor::White);
        let island_a = s.add_basic_land(P0, ManaColor::Blue);
        let island_b = s.add_basic_land(P0, ManaColor::Blue);
        let restricted_land =
            extra.map(|_| s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id());
        let toluz = s
            .add_creature_to_hand_from_oracle(P0, "Toluz, Clever Conductor", 3, 1, TOLUZ)
            .as_legendary()
            .with_subtypes(vec!["Human", "Rogue"])
            .with_mana_cost(parse_mtgjson_mana_cost("{W/U}{U}{U/B}"))
            .id();
        let mut r = s.build();
        evaluate_layers(r.state_mut());
        if krrik {
            assert!(
                engine::game::static_abilities::player_life_payment_colors(r.state(), P0)
                    .contains(ManaColor::Black)
            );
            assert_eq!(
                engine::game::life_costs::max_phyrexian_life_payments(r.state(), P0),
                0
            );
        }
        for (source, color) in [
            (plains, ManaType::White),
            (island_a, ManaType::Blue),
            (island_b, ManaType::Blue),
        ] {
            let (_, _, grouped) = legal_actions_full(r.state());
            let action = grouped[&source]
                .iter()
                .find(|action| {
                    matches!(action,
                    GameAction::TapLandForMana { selection }
                        if selection.mana_type == color && selection.restrictions.is_empty())
                })
                .expect("the printed basic-land ability offers unrestricted mana")
                .clone();
            r.act(action).unwrap();
        }
        if let (Some(source), Some(color)) = (restricted_land, extra) {
            let (_, _, grouped) = legal_actions_full(r.state());
            let action = grouped[&source]
                .iter()
                .find(|action| {
                    matches!(action,
                    GameAction::TapLandForMana { selection }
                        if selection.mana_type == color
                            && !selection.restrictions.is_empty()
                            && selection.atomic_combination.as_ref()
                                .is_some_and(|units| units.len() == 2))
                })
                .expect("the real Lute ability offers two restricted units")
                .clone();
            r.act(action).unwrap();
        }
        assert!([plains, island_a, island_b]
            .iter()
            .all(|id| r.state().objects[id].tapped));
        assert!(restricted_land.is_none_or(|id| r.state().objects[&id].tapped));
        assert!(activatable_mana_source_selections(r.state(), P0).is_empty());
        let before: Vec<_> = r.state().players[0].mana_pool.units().collect();
        assert_eq!(before.len(), 3 + 2 * usize::from(extra.is_some()));
        assert_eq!(
            before
                .iter()
                .filter(|unit| unit.restrictions.is_empty() && unit.color == ManaType::White)
                .count(),
            1
        );
        assert_eq!(
            before
                .iter()
                .filter(|unit| unit.restrictions.is_empty() && unit.color == ManaType::Blue)
                .count(),
            2
        );

        if pinned {
            let pip_id = before
                .iter()
                .find(|unit| unit.source_id == island_a)
                .unwrap()
                .pip_id;
            r.act(GameAction::CastSpell {
                object_id: toluz,
                card_id: r.state().objects[&toluz].card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Manual,
            })
            .unwrap();
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::ManaPayment { player: P0, .. }
            ));
            r.act(GameAction::SpendPoolMana { pip_id }).unwrap();
            assert!(r
                .state()
                .pending_cast
                .as_ref()
                .unwrap()
                .pinned_pool_units
                .contains(&pip_id));
            r.act(GameAction::PassPriority).unwrap();
            assert!(!r.state().players[0]
                .mana_pool
                .units()
                .any(|unit| unit.pip_id == pip_id));
        } else {
            r.cast(toluz).commit();
        }
        // Deliberately stop at the fully paid casting boundary; Toluz's
        // enters/connive resolution is not the behavior under test.
        assert_eq!(r.state().objects[&toluz].zone, Zone::Stack);
        // K'rrik's printed black-spell trigger is also on the stack.
        assert_eq!(r.state().stack.len(), 1 + usize::from(krrik));
        if krrik {
            assert_eq!(r.state().players[0].life, 1);
        }
        assert_eq!(r.state().objects[&toluz].mana_spent_to_cast_amount, 3);
        assert_eq!(
            r.state().objects[&toluz].mana_spent_source_snapshots.len(),
            3
        );
        let remaining: Vec<_> = r.state().players[0].mana_pool.units().collect();
        assert_eq!(remaining.len(), 2 * usize::from(extra.is_some()));
        assert!(remaining
            .iter()
            .all(|unit| Some(unit.source_id) == restricted_land
                && Some(unit.color) == extra
                && !unit.restrictions.is_empty()));
        assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
    }
}

#[test]
fn conditional_counted_bundle_refills_after_its_power_condition_disappears() {
    // CR 605.3b + CR 608.2h: Remote Farm's complete resolution sacrifices
    // the animated land before Caryatid reads its four-power condition.
    for depletion in [1, 2] {
        let mut s = board();
        let caryatid = s.add_creature_from_oracle(P0, "Ilysian Caryatid", 1, 1,
            "{T}: Add one mana of any color. If you control a creature with power 4 or greater, add two mana of any one color instead.")
            .with_subtypes(vec!["Plant"]).with_mana_cost(parse_mtgjson_mana_cost("{1}{G}")).id();
        let farm = s.add_land_from_oracle(P0, "Remote Farm",
            "This land enters tapped with two depletion counters on it.\n{T}, Remove a depletion counter from this land: Add {W}{W}. If there are no depletion counters on this land, sacrifice it.").id();
        s.with_counter(
            farm,
            CounterType::Generic("depletion".to_owned()),
            depletion,
        );
        let island = s.add_basic_land(P0, ManaColor::Blue);
        let forests: Vec<_> = (0..5)
            .map(|_| s.add_basic_land(P0, ManaColor::Green))
            .collect();
        let awakening = spell(&mut s, "Awakening of Vitu-Ghazi", true,
            "Put nine +1/+1 counters on target land you control. It becomes a legendary 0/0 Elemental creature with haste named Vitu-Ghazi. It's still a land.", "{3}{G}{G}");
        let absorb = spell(
            &mut s,
            "Absorb",
            true,
            "Counter target spell. You gain 3 life.",
            "{W}{U}{U}",
        );
        let opt = s.add_spell_to_hand_from_oracle(P1, "Opt", true,
            "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.")
            .with_mana_cost(parse_mtgjson_mana_cost("{U}")).id();
        s.add_basic_land(P1, ManaColor::Blue);
        let mut r = s.build();
        // An established Farm has untapped on a later turn; its remaining
        // depletion counters above describe either side of the last-use case.
        r.state_mut().objects.get_mut(&farm).unwrap().tapped = false;
        // Float only these printed Forest abilities so the setup spell cannot
        // consume Remote Farm, Caryatid or the Island needed by the payment.
        for forest in &forests {
            let (_, _, grouped) = legal_actions_full(r.state());
            let action = grouped[forest]
                .iter()
                .find(|action| {
                    matches!(action,
                GameAction::TapLandForMana { selection } if selection.mana_type == ManaType::Green)
                })
                .unwrap()
                .clone();
            r.act(action).unwrap();
        }
        r.cast(awakening)
            .target_object(farm)
            .resolve()
            .assert_zone(&[farm], Zone::Battlefield);
        assert_complete(&r, 0);
        assert!(forests.iter().all(|id| r.state().objects[id].tapped));
        assert!(r.state().objects[&farm]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature));
        assert_eq!(
            r.state().objects[&farm]
                .counters
                .get(&CounterType::Generic("depletion".to_owned())),
            Some(&depletion)
        );
        assert!(activatable_mana_options(r.state(), caryatid, P0)
            .iter()
            .any(|row| row.object_id == caryatid
                && row.mana_type == ManaType::Blue
                && row
                    .atomic_combination
                    .as_ref()
                    .is_some_and(|units| units == &[ManaType::Blue, ManaType::Blue])));
        assert!(activatable_land_mana_options(r.state(), farm, P0)
            .iter()
            .any(|row| row.mana_type == ManaType::White
                && row
                    .atomic_combination
                    .as_ref()
                    .is_some_and(|units| units == &[ManaType::White, ManaType::White])));
        assert!(!r.state().objects[&island].tapped);
        r.act(GameAction::PassPriority).unwrap();
        let mut target = r.cast(opt).commit();
        target.act(GameAction::PassPriority).unwrap();
        let payment = target.cast(absorb).target_object(opt).commit();
        assert_eq!(production_count(payment.events(), farm, ManaType::White), 2);
        assert_eq!(
            production_count(payment.events(), caryatid, ManaType::Blue),
            if depletion == 1 { 1 } else { 2 }
        );
        assert_eq!(
            production_count(payment.events(), island, ManaType::Blue),
            usize::from(depletion == 1)
        );
        assert_eq!(payment.state().objects[&island].tapped, depletion == 1);
        assert_eq!(
            payment.state().objects[&farm].zone,
            if depletion == 1 {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(
            payment.state().objects[&absorb].mana_spent_to_cast_amount,
            3
        );
        assert_eq!(payment.state().players[0].mana_pool.total(), 1);
        let resolved = payment.resolve();
        resolved.assert_zone(&[absorb, opt], Zone::Graveyard);
        resolved.assert_life_delta(P0, 3);
        assert_complete(&r, 1);
    }
}

#[test]
fn restricted_lute_alternatives_preserve_native_mountain_activation_rows() {
    // Preservation control for mixed projected/scalar snapshots. CR 106.6:
    // Lute's instant/sorcery-only mana cannot pay Shivan Dragon's ability.
    let mut s = board();
    s.add_artifact_from_oracle(P0, "Resonating Lute", LUTE)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{U}{R}"));
    let mountains: Vec<_> = (0..16)
        .map(|_| s.add_basic_land(P0, ManaColor::Red))
        .collect();
    let dragon = s
        .add_creature_from_oracle(
            P0,
            "Shivan Dragon",
            5,
            5,
            "Flying\n{R}: This creature gets +1/+0 until end of turn.",
        )
        .with_subtypes(vec!["Dragon"])
        .with_mana_cost(parse_mtgjson_mana_cost("{4}{R}{R}"))
        .id();
    let mut r = s.build();
    evaluate_layers(r.state_mut());
    let rows = activatable_land_mana_options(r.state(), mountains[0], P0);
    let native = rows
        .iter()
        .find(|row| {
            row.mana_type == ManaType::Red
                && row.atomic_combination.is_none()
                && row.restrictions.is_empty()
        })
        .unwrap();
    let granted = rows
        .iter()
        .find(|row| {
            row.mana_type == ManaType::Red
                && row
                    .atomic_combination
                    .as_ref()
                    .is_some_and(|units| units.len() == 2)
                && !row.restrictions.is_empty()
        })
        .unwrap();
    assert_ne!(native.ability_index, granted.ability_index);
    let abilities = activated_ability_definitions(r.state(), dragon);
    assert_eq!(abilities.len(), 1);
    let index = abilities[0].0;
    for paid in 1..=4 {
        let outcome = r.activate(dragon, index).resolve();
        assert_eq!(
            mountains
                .iter()
                .map(|source| production_count(outcome.events(), *source, ManaType::Red))
                .sum::<usize>(),
            1
        );
        assert_eq!(
            mountains
                .iter()
                .filter(|id| r.state().objects[id].tapped)
                .count(),
            paid
        );
        assert_complete(&r, 0);
    }
}

#[test]
fn live_life_total_bundle_refills_only_the_actual_absorb_shortfall() {
    for life in [2, 3] {
        let mut s = board();
        s.with_life(P0, life);
        let wastes = s
            .add_land_from_oracle(
                P0,
                "Adarkar Wastes",
                "{T}: Add {C}.\n{T}: Add {W} or {U}. This land deals 1 damage to you.",
            )
            .id();
        let sanctum = s.add_land_from_oracle(P0, "Sanctum of the Sun",
            "(Transforms from Azor's Gateway.)\n{T}: Add X mana of any one color, where X is your life total.").as_legendary().id();
        let island = s.add_basic_land(P0, ManaColor::Blue);
        let opponent_island = s.add_basic_land(P1, ManaColor::Blue);
        let opt = s.add_spell_to_hand_from_oracle(P1, "Opt", true,
            "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.")
            .with_mana_cost(parse_mtgjson_mana_cost("{U}")).id();
        let absorb = spell(
            &mut s,
            "Absorb",
            true,
            "Counter target spell. You gain 3 life.",
            "{W}{U}{U}",
        );
        let mut r = s.build();
        assert!(activatable_land_mana_options(r.state(), sanctum, P0)
            .iter()
            .any(|row| row.mana_type == ManaType::Blue
                && row
                    .atomic_combination
                    .as_ref()
                    .is_some_and(|units| units.len() == life as usize)));
        r.act(GameAction::PassPriority).unwrap();
        let mut opt_cast = r.cast(opt).commit();
        assert_eq!(opt_cast.state().stack.len(), 1);
        opt_cast.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            opt_cast.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        let payment = opt_cast.cast(absorb).target_object(opt).commit();
        // CR 120.3a + CR 608.2h: Wastes deals its damage before Sanctum
        // reads life. A completed live bundle triggers one residual refresh.
        assert_eq!(payment.state().players[0].life, life - 1);
        assert_eq!(
            production_count(payment.events(), wastes, ManaType::White),
            1
        );
        assert_eq!(
            production_count(payment.events(), sanctum, ManaType::Blue),
            (life - 1) as usize
        );
        assert_eq!(
            production_count(payment.events(), island, ManaType::Blue),
            usize::from(life == 2)
        );
        assert_eq!(
            production_count(payment.events(), opponent_island, ManaType::Blue),
            0
        );
        assert_eq!(payment.state().objects[&island].tapped, life == 2);
        // CR 601.2h: the printed W/U/U cost is fully spent at commitment.
        assert_eq!(
            payment.state().objects[&absorb].mana_spent_to_cast_amount,
            3
        );
        assert_eq!(
            payment.state().objects[&absorb]
                .mana_spent_source_snapshots
                .len(),
            3
        );
        assert_eq!(payment.state().players[0].mana_pool.total(), 0);
        payment
            .resolve()
            .assert_zone(&[absorb, opt], Zone::Graveyard);
        assert_eq!(opt_cast.state().players[0].life, life + 2);
        drop(opt_cast);
        assert_complete(&r, 0);
    }
}

#[test]
fn absorb_without_a_source_for_both_colors_is_refused_before_payment() {
    let mut s = board();
    s.with_life(P0, 2);
    let sanctum = s.add_land_from_oracle(P0, "Sanctum of the Sun",
        "(Transforms from Azor's Gateway.)\n{T}: Add X mana of any one color, where X is your life total.").as_legendary().id();
    let island = s.add_basic_land(P0, ManaColor::Blue);
    s.add_basic_land(P1, ManaColor::Blue);
    let opt = s.add_spell_to_hand_from_oracle(P1, "Opt", true,
        "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.")
        .with_mana_cost(parse_mtgjson_mana_cost("{U}")).id();
    let absorb = spell(
        &mut s,
        "Absorb",
        true,
        "Counter target spell. You gain 3 life.",
        "{W}{U}{U}",
    );
    let mut r = s.build();
    let rows = activatable_land_mana_options(r.state(), sanctum, P0);
    assert!([ManaType::White, ManaType::Blue]
        .iter()
        .all(|color| rows.iter().any(|row| row.mana_type == *color
            && row
                .atomic_combination
                .as_ref()
                .is_some_and(|units| units.len() == 2))));
    r.act(GameAction::PassPriority).unwrap();
    let mut opt_cast = r.cast(opt).commit();
    opt_cast.act(GameAction::PassPriority).unwrap();
    let result = opt_cast.cast(absorb).target_object(opt).try_resolve();
    // CR 601.2h: two same-color Sanctum units plus one U cannot pay W/U/U.
    assert!(result.is_err());
    assert_eq!(opt_cast.state().stack.len(), 1);
    assert_eq!(opt_cast.state().objects[&absorb].zone, Zone::Hand);
    assert!([sanctum, island]
        .iter()
        .all(|id| !opt_cast.state().objects[id].tapped));
    assert!(opt_cast.state().players[0].mana_pool.is_empty());
}

#[test]
fn manual_sanctum_first_preserves_the_live_color_prompt_and_payment() {
    let mut s = board();
    s.with_life(P0, 2);
    let sanctum = s.add_land_from_oracle(P0, "Sanctum of the Sun",
        "(Transforms from Azor's Gateway.)\n{T}: Add X mana of any one color, where X is your life total.").as_legendary().id();
    let wastes = s
        .add_land_from_oracle(
            P0,
            "Adarkar Wastes",
            "{T}: Add {C}.\n{T}: Add {W} or {U}. This land deals 1 damage to you.",
        )
        .id();
    s.add_basic_land(P1, ManaColor::Blue);
    let opt = s.add_spell_to_hand_from_oracle(P1, "Opt", true,
        "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.")
        .with_mana_cost(parse_mtgjson_mana_cost("{U}")).id();
    let absorb = spell(
        &mut s,
        "Absorb",
        true,
        "Counter target spell. You gain 3 life.",
        "{W}{U}{U}",
    );
    let mut r = s.build();
    r.act(GameAction::PassPriority).unwrap();
    r.cast(opt).commit();
    r.act(GameAction::PassPriority).unwrap();
    let index = assert_any_one(&r, sanctum);
    r.act(GameAction::ActivateAbility {
        source_id: sanctum,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    })
    .unwrap();
    choose_color(&mut r, ManaType::Blue);
    // CR 608.2d + CR 608.2h: manual Sanctum-first measures two life
    // before Wastes deals its damage. It remains a legal independent order.
    assert_eq!(
        r.state().players[0].mana_pool.count_color(ManaType::Blue),
        2
    );
    assert_eq!(r.state().players[0].life, 2);
    float_land(&mut r, wastes, ManaType::White);
    assert_eq!(r.state().players[0].life, 1);
    r.cast(absorb)
        .target_object(opt)
        .resolve()
        .assert_zone(&[opt, absorb], Zone::Graveyard);
    assert_eq!(r.state().players[0].life, 4);
    assert_complete(&r, 0);
}

#[test]
fn partial_snow_hybrid_pool_chooses_the_residual_the_board_can_fund() {
    // Supplemental component-composition costs: Astrolabe's S, Familiar's
    // W/U and Ulalek's C/U; no printing combines these with the strict U.
    for colorless_hybrid in [false, true] {
        for spare_kind in [0, 1, 2] {
            for snow_first in [false, true] {
                let mut s = board();
                let snow = if colorless_hybrid {
                    s.add_land_from_oracle(P0, "Faceless Haven", "{T}: Add {C}.\n{S}{S}{S}: This land becomes a 4/3 creature with vigilance and all creature types until end of turn. It's still a land. ({S} can be paid with one mana from a snow source.)").as_snow().id()
                } else {
                    s.add_land_from_oracle(P0, "Snow-Covered Plains", "({T}: Add {W}.)")
                        .as_snow()
                        .with_subtypes(vec!["Plains"])
                        .id()
                };
                let island = s.add_basic_land(P0, ManaColor::Blue);
                let spare = match spare_kind {
                    0 => None,
                    1 => Some(s.add_basic_land(P0, ManaColor::Blue)),
                    _ => Some(s.add_land_from_oracle(P0, "Faceless Haven", "{T}: Add {C}.\n{S}{S}{S}: This land becomes a 4/3 creature with vigilance and all creature types until end of turn. It's still a land. ({S} can be paid with one mana from a snow source.)").as_snow().id()),
                };
                let symbol = if colorless_hybrid { "{C/U}" } else { "{W/U}" };
                let cost = if snow_first {
                    format!("{{S}}{symbol}{{U}}")
                } else {
                    format!("{symbol}{{S}}{{U}}")
                };
                let astrolabe = s.add_artifact_to_hand_from_oracle(P0, "Arcum's Astrolabe",
                    "({S} can be paid with one mana from a snow source.)\nWhen this artifact enters, draw a card.\n{1}, {T}: Add one mana of any color.")
                    .as_snow().with_mana_cost(parse_mtgjson_mana_cost(&cost)).id();
                let mut r = s.build();
                finish_snow_forest(&mut r, snow);
                if spare_kind == 2 {
                    finish_snow_forest(&mut r, spare.unwrap());
                }
                evaluate_layers(r.state_mut());
                float_land(
                    &mut r,
                    snow,
                    if colorless_hybrid {
                        ManaType::Colorless
                    } else {
                        ManaType::White
                    },
                );
                float_land(&mut r, island, ManaType::Blue);
                let pool = &r.state().players[0].mana_pool;
                assert_eq!(pool.total(), 2);
                assert_eq!(
                    pool.units()
                        .filter(|unit| unit.source_id == snow
                            && unit.supertype == Some(ManaSupertype::Snow))
                        .count(),
                    1
                );
                if let Some(spare) = spare {
                    assert!(!r.state().objects[&spare].tapped);
                }
                // CR 107.4e + CR 107.4h: the legal allocation depends on
                // the remaining source; both complementary boards must pay.
                if let Some(spare) = spare {
                    r.cast(astrolabe).resolve().assert_hand_drawn(P0, 1);
                    assert_eq!(r.state().objects[&astrolabe].zone, Zone::Battlefield);
                    assert!(r.state().objects[&spare].tapped);
                    assert_complete(&r, 0);
                } else {
                    // CR 601.2h: the reached pool has only two units for
                    // three symbols when there is no complementary source.
                    assert!(r.cast(astrolabe).try_resolve().is_err());
                    assert_eq!(r.state().objects[&astrolabe].zone, Zone::Hand);
                    assert_complete(&r, 2);
                }
            }
        }
    }
}

fn float_land(
    r: &mut GameRunner,
    source: ObjectId,
    color: ManaType,
) -> Vec<engine::types::events::GameEvent> {
    let (_, _, grouped) = legal_actions_full(r.state());
    let action = grouped[&source]
        .iter()
        .find(|action| {
            matches!(action,
        GameAction::TapLandForMana { selection } if selection.mana_type == color)
        })
        .expect("the printed land offers its selected public mana action")
        .clone();
    r.act(action).unwrap().events
}

fn production_count(
    events: &[engine::types::events::GameEvent],
    source: ObjectId,
    color: ManaType,
) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event,
        engine::types::events::GameEvent::ManaAdded { source_id, mana_type, .. }
        if *source_id == source && *mana_type == color)
        })
        .count()
}

#[test]
fn scalar_counted_output_keeps_the_separate_same_color_snow_producer() {
    for ice_land in [None, Some(false), Some(true)] {
        let mut s = board();
        let owl = owl(&mut s);
        let setup_snow = s
            .add_land_from_oracle(P0, "Snow-Covered Island", "({T}: Add {U}.)")
            .as_snow()
            .with_subtypes(vec!["Island"])
            .id();
        let setup_c = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
        let floating = [
            s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id(),
            s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id(),
            s.add_basic_land(P0, ManaColor::White),
            s.add_basic_land(P0, ManaColor::Red),
        ];
        let signet = s
            .add_artifact_from_oracle(P0, "Dimir Signet", "{1}, {T}: Add {U}{B}.")
            .with_mana_cost(parse_mtgjson_mana_cost("{2}"))
            .id();
        let land = s.add_land_from_oracle(P0, "Lotus Field", LOTUS_FIELD).id();
        let aura = s
            .add_enchantment_from_oracle(P0, "Wild Growth", WILD_GROWTH)
            .with_subtypes(vec!["Aura"])
            .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
            .id();
        let wurm = s
            .add_creature_to_hand_from_oracle(P0, "Wowzer, the Aspirational", 10, 10, WOWZER)
            .as_legendary()
            .as_snow()
            .with_subtypes(vec!["Wurm"])
            .with_mana_cost(parse_mtgjson_mana_cost("{C}{W}{U}{B}{R}{G}{S}"))
            .id();
        let mut r = s.build();
        r.state_mut().objects.get_mut(&land).unwrap().tapped = false;
        engine::game::effects::attach::attach_to(r.state_mut(), aura, land);
        let target = ice_land.map_or(owl, |land_is_snow| if land_is_snow { land } else { aura });
        ice_permanent(&mut r, owl, target, setup_snow, setup_c);
        for (source, color) in floating.into_iter().zip([
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::Red,
        ]) {
            float_land(&mut r, source, color);
        }
        assert_eq!(r.state().players[0].mana_pool.total(), 4);
        assert!(r.state().players[0]
            .mana_pool
            .units()
            .all(|unit| unit.supertype.is_none()));
        assert!([land, signet]
            .iter()
            .all(|id| !r.state().objects[id].tapped));
        let rows = activatable_land_mana_options(r.state(), land, P0);
        assert!(rows.iter().any(|row| row.mana_type == ManaType::Green
            && row
                .atomic_combination
                .as_ref()
                .is_some_and(|units| units == &vec![ManaType::Green; 4])
            && row.taps_for_mana_overrides.len() == 1
            && row.taps_for_mana_overrides[0].0.source.object_id == aura));
        if let Some(land_is_snow) = ice_land {
            let paid = r.cast(wurm).commit();
            // CR 106.3 + CR 107.4h: scalar credit limits only the base
            // output. Equal-colored Aura and Lotus mana retain their sources.
            let base_colors: Vec<_> = [
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ]
            .into_iter()
            .filter(|&color| production_count(paid.events(), land, color) > 0)
            .collect();
            assert_eq!(base_colors.len(), 1);
            let base_color = base_colors[0];
            assert_eq!(production_count(paid.events(), land, base_color), 3);
            if !land_is_snow {
                // Only the Aura is Snow: Lotus must provide the printed G.
                assert_eq!(base_color, ManaType::Green);
            }
            assert_eq!(production_count(paid.events(), aura, ManaType::Green), 1);
            assert_eq!(production_count(paid.events(), signet, ManaType::Blue), 1);
            assert_eq!(production_count(paid.events(), signet, ManaType::Black), 1);
            assert_eq!(paid.state().objects[&wurm].mana_spent_to_cast_amount, 7);
            assert_eq!(paid.state().players[0].mana_pool.total(), 2);
            assert!(paid.state().players[0]
                .mana_pool
                .units()
                .all(|unit| unit.source_id == land
                    && unit.color == base_color
                    && unit.supertype == land_is_snow.then_some(ManaSupertype::Snow)));
            paid.resolve().assert_zone(&[wurm], Zone::Battlefield);
            assert!([land, signet].iter().all(|id| r.state().objects[id].tapped));
            assert_complete(&r, 2);
        } else {
            // CR 107.4h + CR 601.2h: the paired board has every printed
            // color and enough total mana, but neither available producer is Snow.
            assert!(r.cast(wurm).try_resolve().is_err());
            assert!([land, signet]
                .iter()
                .all(|id| !r.state().objects[id].tapped));
            assert_eq!(r.state().objects[&wurm].zone, Zone::Hand);
            assert_complete(&r, 4);
        }
    }
}

#[test]
fn overgrowth_keeps_both_snow_units_without_tapping_the_spare() {
    for snow_aura in [false, true] {
        let mut s = board();
        let owl = owl(&mut s);
        let setup_snow = s
            .add_land_from_oracle(P0, "Snow-Covered Island", "({T}: Add {U}.)")
            .as_snow()
            .with_subtypes(vec!["Island"])
            .id();
        let setup_c = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
        let land = s.add_land_from_oracle(P0, "Lotus Field", LOTUS_FIELD).id();
        let aura = s
            .add_enchantment_from_oracle(P0, "Overgrowth", OVERGROWTH)
            .with_subtypes(vec!["Aura"])
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{G}"))
            .id();
        let spare = s
            .add_land_from_oracle(P0, "Snow-Covered Island", "({T}: Add {U}.)")
            .as_snow()
            .with_subtypes(vec!["Island"])
            .id();
        let raptor = s
            .add_creature_from_oracle(P0, "Frost Raptor", 2, 2, RAPTOR)
            .as_snow()
            .with_subtypes(vec!["Bird"])
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{U}"))
            .id();
        let mut r = s.build();
        r.state_mut().objects.get_mut(&land).unwrap().tapped = false;
        engine::game::effects::attach::attach_to(r.state_mut(), aura, land);
        finish_snow_forest(&mut r, spare);
        ice_permanent(
            &mut r,
            owl,
            if snow_aura { aura } else { owl },
            setup_snow,
            setup_c,
        );
        let rows = activatable_land_mana_options(r.state(), land, P0);
        assert!(rows.iter().any(|row| row.taps_for_mana_overrides.len() == 1
            && row.taps_for_mana_overrides[0].0.source.object_id == aura));
        let result = r.act(GameAction::ActivateAbility {
            source_id: raptor,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        });
        if snow_aura {
            let result = result.expect("one Overgrowth producer pays both Snow symbols");
            // CR 106.3 + CR 107.4h: one trigger has two Snow units; it
            // must not be reduced to the one color-selection marker.
            assert_eq!(production_count(&result.events, aura, ManaType::Green), 2);
            assert!(!r.state().objects[&spare].tapped);
            assert!(r.state().objects[&land].tapped);
            assert_eq!(r.state().stack.len(), 1);
            r.advance_until_stack_empty();
            // CR 702.18a: the paid printed Raptor ability actually resolves.
            assert!(
                r.state().objects[&raptor].has_keyword(&engine::types::keywords::Keyword::Shroud)
            );
            assert!(r.state().players[0]
                .mana_pool
                .units()
                .all(|unit| unit.source_id == land && unit.supertype.is_none()));
            assert_complete(&r, 3);
        } else {
            // CR 107.4h: the lone Snow Island cannot pay two S while
            // Overgrowth and Lotus remain ordinary producers.
            assert!(result.is_err());
            assert!([land, spare].iter().all(|id| !r.state().objects[id].tapped));
            assert_complete(&r, 0);
        }
    }
}

#[test]
fn distinct_same_color_aura_sources_keep_their_own_controller_and_snow_quality() {
    let mut s = board();
    let owl = owl(&mut s);
    let setup_snow = s
        .add_land_from_oracle(P0, "Snow-Covered Island", "({T}: Add {U}.)")
        .as_snow()
        .with_subtypes(vec!["Island"])
        .id();
    let setup_c = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
    let land = s.add_land_from_oracle(P0, "Lotus Field", LOTUS_FIELD).id();
    let first = s
        .add_enchantment_from_oracle(P0, "Wild Growth", WILD_GROWTH)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
        .id();
    let second = s
        .add_enchantment_from_oracle(P1, "Wild Growth", WILD_GROWTH)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
        .id();
    let spare = s
        .add_land_from_oracle(P0, "Snow-Covered Island", "({T}: Add {U}.)")
        .as_snow()
        .with_subtypes(vec!["Island"])
        .id();
    let raptor = s
        .add_creature_from_oracle(P0, "Frost Raptor", 2, 2, RAPTOR)
        .as_snow()
        .with_subtypes(vec!["Bird"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{U}"))
        .id();
    let mut r = s.build();
    r.state_mut().objects.get_mut(&land).unwrap().tapped = false;
    for aura in [first, second] {
        engine::game::effects::attach::attach_to(r.state_mut(), aura, land);
    }
    finish_snow_forest(&mut r, spare);
    ice_permanent(&mut r, owl, second, setup_snow, setup_c);
    let result = r
        .act(GameAction::ActivateAbility {
            source_id: raptor,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    // CR 106.3 + CR 107.4h: the opponent's Snow Aura produces for the
    // tapped land's controller, but its own source supplies the quality.
    assert_eq!(production_count(&result.events, first, ManaType::Green), 1);
    assert_eq!(production_count(&result.events, second, ManaType::Green), 1);
    assert!(r.state().players[1].mana_pool.is_empty());
    assert!(r.state().objects[&spare].tapped);
    r.advance_until_stack_empty();
    // CR 702.18a: the paid printed Raptor ability actually resolves.
    assert!(r.state().objects[&raptor].has_keyword(&engine::types::keywords::Keyword::Shroud));
    assert_eq!(
        r.state().players[0]
            .mana_pool
            .units()
            .filter(|unit| unit.source_id == first)
            .count(),
        1
    );
    assert!(r.state().players[0]
        .mana_pool
        .units()
        .all(|unit| unit.supertype.is_none()));
    assert_complete(&r, 4);
}

fn nykthos_aura_board(
    paused: bool,
    wastes_count: usize,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut s = board();
    owl(&mut s);
    let nykthos = s
        .add_land_from_oracle(P0, "Nykthos, Shrine to Nyx", NYKTHOS)
        .as_legendary()
        .id();
    let aura = s
        .add_enchantment_from_oracle(P0, "Fertile Ground", FERTILE_GROUND)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    let funders = if paused {
        s.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE)
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{W}"));
        s.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_VOID)
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}{B}"));
        vec![s
            .add_creature_from_oracle(P0, "Basal Thrull", 1, 2, BASAL_THRULL)
            .with_subtypes(vec!["Thrull"])
            .with_mana_cost(parse_mtgjson_mana_cost("{B}{B}"))
            .id()]
    } else {
        (0..wastes_count)
            .map(|_| s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id())
            .collect()
    };
    let spiral = spell(&mut s, "Growth Spiral", true, SPIRAL, "{G}{U}");
    let mut r = s.build();
    for source in &funders {
        r.state_mut().objects.get_mut(source).unwrap().tapped = false;
    }
    engine::game::effects::attach::attach_to(r.state_mut(), aura, nykthos);
    evaluate_layers(r.state_mut());
    // CR 700.5 + CR 608.2h: the printed Owl's two U symbols supply
    // devotion two; Fertile Ground supplies one G. Opponent costs do not count.
    assert!(r.state().players[0].mana_pool.is_empty());
    assert_eq!(
        engine::game::devotion::count_devotion(r.state(), P0, &[ManaColor::Blue]),
        2
    );
    let (_, ability) = activated_ability_definitions(r.state(), nykthos)
        .into_iter()
        .find(|(_, ability)| {
            matches!(
                &*ability.effect,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor { .. },
                    ..
                }
            )
        })
        .expect("the printed chosen-color devotion ability is present");
    assert!(
        matches!(&ability.cost, Some(AbilityCost::Composite { costs })
        if costs.len() == 2
            && costs.iter().any(|cost| matches!(cost, AbilityCost::Tap))
            && costs.iter().any(|cost| matches!(cost,
                AbilityCost::Mana { cost } if *cost == parse_mtgjson_mana_cost("{2}"))))
    );
    let rows = activatable_land_mana_options(r.state(), nykthos, P0);
    assert!(rows
        .iter()
        .any(|row| row.mana_type == ManaType::Colorless && row.taps_for_mana_overrides.len() == 1));
    if !paused && wastes_count >= 2 {
        assert!(
            rows.iter().any(|row| row.mana_type == ManaType::Blue
                && row.atomic_combination.as_ref() == Some(&vec![ManaType::Blue, ManaType::Green])
                && row.taps_for_mana_overrides.len() == 1
                && row.taps_for_mana_overrides[0].0.source.object_id == aura
                && row.taps_for_mana_overrides[0].1
                    == engine::types::game_state::ProductionOverride::SingleColor(ManaType::Green)),
            "currently payable choices: {rows:?}"
        );
    } else if !paused {
        assert!(!rows.iter().any(|row| row.mana_type == ManaType::Blue));
        assert_eq!(funders.len(), 1);
        assert!(activatable_land_mana_options(r.state(), funders[0], P0)
            .iter()
            .any(|row| row.mana_type == ManaType::Colorless));
    }
    (r, nykthos, aura, spiral, funders)
}

fn assert_replacement_pair(r: &GameRunner) {
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        candidates,
        ..
    } = &r.state().waiting_for
    else {
        panic!(
            "the real sacrifice/finish must reach a replacement-order choice, got {:?}",
            r.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*candidate_count, 2);
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_name == "Rest in Peace"));
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_name == "Leyline of the Void"));
}

fn finish_growth_spiral(r: &mut GameRunner, spiral: ObjectId, exiled: bool, remaining: usize) {
    let library_before = r.state().players[0].library.len();
    assert_eq!(r.state().objects[&spiral].zone, Zone::Stack);
    r.resolve_top();
    // Growth Spiral may offer its optional land-put after the real draw.
    // Handle each observed continuation explicitly, including the independent
    // post-resolution graveyard replacement under RIP/Leyline.
    for _ in 0..4 {
        match &r.state().waiting_for {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(*player, P0);
                r.act(GameAction::DecideOptionalEffect { accept: false })
                    .unwrap();
            }
            WaitingFor::EffectZoneChoice {
                player, min_count, ..
            } => {
                assert_eq!(*player, P0);
                assert_eq!(*min_count, 0);
                r.act(GameAction::SelectCards { cards: vec![] }).unwrap();
            }
            WaitingFor::ReplacementChoice { .. } => {
                assert_replacement_pair(r);
                r.act(GameAction::ChooseReplacement { index: 1 }).unwrap();
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unanswered Growth Spiral continuation: {other:?}"),
        }
    }
    // CR 608.2n + CR 614.1a + CR 616.1: RIP/Leyline also replace
    // the resolved instant's graveyard move; one draw happens before that move.
    assert_eq!(r.state().players[0].library.len(), library_before - 1);
    assert_eq!(
        r.state().objects[&spiral].zone,
        if exiled { Zone::Exile } else { Zone::Graveyard }
    );
    assert_complete(r, remaining);
}

#[test]
fn costed_counted_source_keeps_nondefault_aura_through_actual_payment() {
    let (mut r, nykthos, aura, spiral, funders) = nykthos_aura_board(false, 2);
    let paid = r.cast(spiral).commit();
    // CR 118.10 + CR 605.4a: independent C/C pays the activation once,
    // then Nykthos produces UU and its independently chosen Aura produces G.
    for source in &funders {
        assert_eq!(
            production_count(paid.events(), *source, ManaType::Colorless),
            1
        );
        assert!(paid.state().objects[source].tapped);
    }
    assert_eq!(production_count(paid.events(), nykthos, ManaType::Blue), 2);
    assert_eq!(production_count(paid.events(), aura, ManaType::Green), 1);
    assert_eq!(production_count(paid.events(), aura, ManaType::White), 0);
    assert!(paid.state().objects[&nykthos].tapped);
    assert_eq!(
        paid.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(nykthos))
    );
    // CR 601.2h: the spell spent G/U, leaving exactly one Nykthos U.
    assert_eq!(paid.state().objects[&spiral].mana_spent_to_cast_amount, 2);
    assert_eq!(
        paid.state().objects[&spiral]
            .mana_spent_source_snapshots
            .iter()
            .filter(|snapshot| snapshot.source_id == aura)
            .count(),
        1
    );
    assert!(paid.state().players[0]
        .mana_pool
        .units()
        .all(|unit| unit.source_id == nykthos && unit.color == ManaType::Blue));
    drop(paid);
    finish_growth_spiral(&mut r, spiral, false, 1);
}

#[test]
fn costed_counted_source_cannot_fund_itself_from_future_aura_output() {
    let (mut r, nykthos, _aura, spiral, funders) = nykthos_aura_board(false, 1);
    let (_, _, grouped) = legal_actions_full(r.state());
    assert!(grouped[&funders[0]].iter().any(
        |action| matches!(action, GameAction::TapLandForMana { selection }
        if selection.mana_type == ManaType::Colorless)
    ));
    // CR 118.10 + CR 601.2h: one independently available C cannot pay
    // the two-mana activation, even though its future output could pay G/U.
    assert!(r.cast(spiral).try_resolve().is_err());
    assert!(!r.state().objects[&nykthos].tapped);
    assert!(!r.state().objects[&funders[0]].tapped);
    assert_eq!(r.state().objects[&spiral].zone, Zone::Hand);
    assert_complete(&r, 0);
}

fn quarry_aura_consent(
    paused: bool,
    aura_color: ManaType,
) -> (
    GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
    engine::types::mana::ManaSourceSelection,
) {
    let mut s = board();
    let quarry = s.add_land_from_oracle(P0, "Lazotep Quarry", QUARRY).id();
    s.add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"));
    let aura = s
        .add_enchantment_from_oracle(P0, "Fertile Ground", FERTILE_GROUND)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    if paused {
        s.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE)
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{W}"));
        s.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_VOID)
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}{B}"));
    }
    let spiral = spell(&mut s, "Growth Spiral", true, SPIRAL, "{G}{U}");
    let mut r = s.build();
    engine::game::effects::attach::attach_to(r.state_mut(), aura, quarry);
    evaluate_layers(r.state_mut());
    let card_id = r.state().objects[&spiral].card_id;
    r.act(GameAction::CastSpell {
        object_id: spiral,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
    })
    .unwrap();
    let WaitingFor::ManaSourceSelection {
        player, options, ..
    } = &r.state().waiting_for
    else {
        panic!(
            "Quarry's printed sacrifice must require consent, got {:?}",
            r.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let selection = options
        .iter()
        .find(|selection| {
            selection.source.object_id == quarry
                && matches!(
                    selection.output,
                    ManaSourceOutput::DeferredColorChoice {
                        quantity: ManaSourceQuantity::Fixed(1)
                    }
                )
                && selection.taps_for_mana.iter().any(|selected| {
                    selected.source.object_id == aura
                        && selected.production_override
                            == engine::types::game_state::ProductionOverride::SingleColor(
                                aura_color,
                            )
                })
        })
        .expect("the offered consent retains the independent Aura choice")
        .clone();
    assert!(!r.state().objects[&quarry].tapped);
    assert!(r.state().players[0].mana_pool.is_empty());
    (r, quarry, aura, spiral, selection)
}

fn pay_quarry_creature(r: &mut GameRunner) -> Vec<engine::types::events::GameEvent> {
    let WaitingFor::PayCost {
        player,
        kind: PayCostKind::Sacrifice,
        count,
        choices,
        ..
    } = &r.state().waiting_for
    else {
        panic!("the selected Quarry activation must ask for its real creature cost");
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(choices.len(), 1);
    let creature = choices[0];
    assert_eq!(r.state().objects[&creature].name, "Grizzly Bears");
    r.act(GameAction::SelectCards {
        cards: vec![creature],
    })
    .unwrap()
    .events
}

#[test]
fn manual_sacrificial_source_keeps_its_independent_aura_and_base_choices() {
    for (base_color, aura_color) in [
        (ManaType::Blue, ManaType::Green),
        (ManaType::Green, ManaType::Blue),
    ] {
        let (mut r, quarry, aura, spiral, selection) = quarry_aura_consent(false, aura_color);
        let mut events = r
            .act(GameAction::ActivateManaSource { selection })
            .unwrap()
            .events;
        events.extend(pay_quarry_creature(&mut r));
        let WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            context: engine::types::game_state::ManaChoiceContext::ManaAbility(pending),
        } = &r.state().waiting_for
        else {
            panic!("the base choice remains mandatory after actual cost payment");
        };
        assert_eq!(*player, P0);
        assert!(options.contains(&base_color));
        assert_eq!(pending.selected_taps_for_mana.len(), 1);
        assert_eq!(
            pending.selected_taps_for_mana[0].production_override,
            engine::types::game_state::ProductionOverride::SingleColor(aura_color)
        );
        assert_eq!(r.state().objects[&quarry].zone, Zone::Battlefield);
        assert!(r.state().objects[&quarry].tapped);
        assert!(r.state().players[0].mana_pool.is_empty());
        events.extend(
            r.act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(base_color),
                count: 1,
            })
            .unwrap()
            .events,
        );
        // CR 608.2d + CR 605.4a: the real manual choice consumer retains
        // the separately selected producer after sacrificing the selected creature.
        assert_eq!(production_count(&events, quarry, base_color), 1);
        assert_eq!(production_count(&events, aura, aura_color), 1);
        assert_eq!(production_count(&events, aura, ManaType::White), 0);
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        r.act(GameAction::PassPriority).unwrap();
        assert_eq!(r.state().objects[&spiral].mana_spent_to_cast_amount, 2);
        finish_growth_spiral(&mut r, spiral, false, 0);
    }
}

#[test]
fn selected_aura_survives_prefunding_pause_roundtrip_and_terminal_siblings() {
    use engine::types::game_state::{GameState, PendingCostMoveResume, ProductionOverride};
    let (mut r, nykthos, aura, spiral, funders) = nykthos_aura_board(true, 0);
    let thrull = funders[0];
    let selected = activatable_land_mana_options(r.state(), nykthos, P0)
        .into_iter()
        .find(|row| {
            row.mana_type == ManaType::Colorless
                && row.taps_for_mana_overrides.iter().any(|(trigger, value)| {
                    trigger.source.object_id == aura
                        && *value == ProductionOverride::SingleColor(ManaType::Green)
                })
        })
        .unwrap()
        .semantic_selection(r.state())
        .unwrap()
        .taps_for_mana;
    let card_id = r.state().objects[&spiral].card_id;
    let mut events = r
        .act(GameAction::CastSpell {
            object_id: spiral,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap()
        .events;
    assert_replacement_pair(&r);
    assert!(r.state().objects[&thrull].tapped);
    assert!(!r.state().objects[&nykthos].tapped);
    assert_eq!(production_count(&events, nykthos, ManaType::Blue), 0);
    assert_eq!(production_count(&events, aura, ManaType::Green), 0);
    assert!(r.state().pending_taps_for_mana_overrides.is_empty());
    let Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) =
        &r.state().pending_cost_move_resume
    else {
        panic!("the Basal Thrull child must park the existing mana-cost cursor");
    };
    assert_eq!(pending.source_id, thrull);
    let parent = cursor
        .parent
        .as_ref()
        .expect("prepared Nykthos exists before prefunding pauses");
    assert_eq!(parent.pending.source_id, nykthos);
    assert_eq!(
        parent.pending.color_override,
        Some(ProductionOverride::SingleColor(ManaType::Blue))
    );
    assert_eq!(parent.pending.selected_taps_for_mana, selected);
    assert_eq!(
        parent.pending.announced_source_incarnation,
        Some(r.state().objects[&nykthos].incarnation)
    );
    assert!(
        !parent.cursor.remaining.is_empty(),
        "prefunding has not paid the prepared activation's costs"
    );
    let wire = serde_json::to_vec(r.state()).unwrap();
    let restored: GameState = serde_json::from_slice(&wire).unwrap();
    assert_eq!(
        serde_json::to_value(&restored.pending_cost_move_resume).unwrap(),
        serde_json::to_value(&r.state().pending_cost_move_resume).unwrap()
    );
    let mut retired = GameRunner::from_state(restored.clone());
    let prior = serde_json::to_value(&retired.state().pending_cost_move_resume).unwrap();
    assert!(retired.act(GameAction::CancelCast).is_err());
    assert!(retired
        .act(GameAction::ChooseReplacement { index: usize::MAX })
        .is_err());
    assert_eq!(
        serde_json::to_value(&retired.state().pending_cost_move_resume).unwrap(),
        prior
    );
    // CR 104.1 + CR 104.3a + CR 800.4a: terminal ownership cannot
    // resume an unproduced selected activation after the player concedes.
    let terminal = retired.act(GameAction::Concede { player_id: P0 }).unwrap();
    assert!(matches!(
        retired.state().waiting_for,
        WaitingFor::GameOver { .. }
    ));
    assert_eq!(
        production_count(&terminal.events, nykthos, ManaType::Blue),
        0
    );
    assert_eq!(production_count(&terminal.events, aura, ManaType::Green), 0);
    assert!(retired
        .act(GameAction::ChooseReplacement { index: 1 })
        .is_err());
    r = GameRunner::from_state(restored);
    assert_replacement_pair(&r);
    events.extend(
        r.act(GameAction::ChooseReplacement { index: 1 })
            .unwrap()
            .events,
    );
    // CR 616.1 + CR 118.10 + CR 605.4a: Basal Thrull's BB funds the
    // activation before its UU and exact Aura G exist, once after restore.
    assert_eq!(r.state().objects[&thrull].zone, Zone::Exile);
    assert_eq!(production_count(&events, thrull, ManaType::Black), 2);
    assert_eq!(production_count(&events, nykthos, ManaType::Blue), 2);
    assert_eq!(production_count(&events, aura, ManaType::Green), 1);
    assert_eq!(production_count(&events, aura, ManaType::White), 0);
    assert_eq!(r.state().objects[&spiral].mana_spent_to_cast_amount, 2);
    assert!(r.state().players[0]
        .mana_pool
        .units()
        .all(|unit| unit.source_id == nykthos && unit.color == ManaType::Blue));
    assert!(r.state().pending_taps_for_mana_overrides.is_empty());
    finish_growth_spiral(&mut r, spiral, true, 1);
}

#[test]
fn manual_selected_aura_survives_actual_cost_pause_and_color_response() {
    let (mut r, quarry, aura, spiral, selection) = quarry_aura_consent(true, ManaType::Green);
    let mut events = r
        .act(GameAction::ActivateManaSource { selection })
        .unwrap()
        .events;
    events.extend(pay_quarry_creature(&mut r));
    assert_replacement_pair(&r);
    assert_eq!(production_count(&events, quarry, ManaType::Blue), 0);
    assert_eq!(production_count(&events, aura, ManaType::Green), 0);
    assert!(r.state().pending_taps_for_mana_overrides.is_empty());
    let restored = serde_json::from_slice(&serde_json::to_vec(r.state()).unwrap()).unwrap();
    r = GameRunner::from_state(restored);
    events.extend(
        r.act(GameAction::ChooseReplacement { index: 1 })
            .unwrap()
            .events,
    );
    let WaitingFor::ChooseManaColor {
        context: engine::types::game_state::ManaChoiceContext::ManaAbility(pending),
        player,
        choice: ManaChoicePrompt::SingleColor { options },
    } = &r.state().waiting_for
    else {
        panic!("resumed actual cost payment must retain the mandatory base choice");
    };
    assert_eq!(*player, P0);
    assert!(options.contains(&ManaType::Blue));
    assert_eq!(pending.selected_taps_for_mana.len(), 1);
    assert_eq!(pending.selected_taps_for_mana[0].source.object_id, aura);
    assert_eq!(
        pending.selected_taps_for_mana[0].production_override,
        engine::types::game_state::ProductionOverride::SingleColor(ManaType::Green)
    );
    assert_eq!(r.state().objects[&quarry].zone, Zone::Battlefield);
    assert!(r.state().objects[&quarry].tapped);
    events.extend(
        r.act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .unwrap()
        .events,
    );
    // CR 608.2d + CR 605.4a + CR 616.1: the resumed manual color
    // consumer preserves the exact selected trigger after replacement.
    assert_eq!(production_count(&events, quarry, ManaType::Blue), 1);
    assert_eq!(production_count(&events, aura, ManaType::Green), 1);
    assert_eq!(production_count(&events, aura, ManaType::White), 0);
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    r.act(GameAction::PassPriority).unwrap();
    finish_growth_spiral(&mut r, spiral, true, 0);
}

fn owl(s: &mut GameScenario) -> ObjectId {
    s.add_creature_from_oracle(P0, "Rimefeather Owl", 1, 1, OWL)
        .as_snow()
        .with_subtypes(vec!["Bird"])
        .with_mana_cost(parse_mtgjson_mana_cost("{5}{U}{U}"))
        .id()
}

fn ice_permanent(
    r: &mut GameRunner,
    owl: ObjectId,
    target: ObjectId,
    snow: ObjectId,
    ordinary: ObjectId,
) {
    finish_snow_forest(r, snow);
    evaluate_layers(r.state_mut());
    float_land(r, snow, ManaType::Blue);
    float_land(r, ordinary, ManaType::Colorless);
    assert_eq!(r.state().players[0].mana_pool.total(), 2);
    r.activate(owl, 0).target_object(target).resolve();
    // CR 205.4g + CR 608.2h: Owl's real ice-counter effect makes this
    // permanent Snow; the other producer does not inherit that quality.
    assert_eq!(
        r.state().objects[&target]
            .counters
            .get(&CounterType::Generic("ice".into())),
        Some(&1)
    );
    assert!(r.state().objects[&target]
        .card_types
        .supertypes
        .contains(&Supertype::Snow));
    assert_complete(r, 0);
}

fn assert_any_one(r: &GameRunner, source: ObjectId) -> usize {
    r.state().objects[&source]
        .abilities
        .iter()
        .position(|a| {
            matches!(
                &*a.effect,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor { .. },
                    ..
                }
            )
        })
        .expect("real Oracle ability must reach AnyOneColor")
}

#[test]
fn activation_color_rider_filters_type_agnostic_source_rows() {
    // CR 106.6: Throne's printed activation permits only its chosen color.
    // The Z variant is supplemental: combine that real rider with Experiment
    // Five's source-quality shard to exercise the empty acceptable-type list.
    // This does not claim that Throne has a printed Z activation.
    const THRONE: &str = "As Throne of Eldraine enters, choose a color.\n{T}: Add four mana of the chosen color. Spend this mana only to cast monocolored spells of that color.\n{3}, {T}: Draw two cards. Spend only mana of the chosen color to activate this ability.";
    for cost in ["{3}", "{Z}"] {
        for with_lotus in [true, false] {
            let mut s = board();
            let throne = s
                .add_creature_from_oracle(
                    P0,
                    "Throne of Eldraine",
                    0,
                    1,
                    &THRONE.replace("{3}", cost),
                )
                .as_artifact()
                .id();
            let boilerworks = s.add_land_from_oracle(P0, "Izzet Boilerworks",
                "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {U}{R}.").id();
            let lotus = with_lotus.then(|| {
                s.add_creature_from_oracle(P0, "Gilded Lotus", 0, 1, LOTUS)
                    .as_artifact()
                    .id()
            });
            let mut r = s.build();
            r.state_mut()
                .objects
                .get_mut(&throne)
                .unwrap()
                .chosen_attributes
                .push(ChosenAttribute::Color(ManaColor::Green));
            // Scenario construction places permanents directly; this fixture
            // starts on a later turn with Boilerworks available for tapping.
            r.state_mut().objects.get_mut(&boilerworks).unwrap().tapped = false;
            let (index, ability) = activated_ability_definitions(r.state(), throne)
                .into_iter()
                .find(|(_, ability)| matches!(ability.effect.as_ref(), Effect::Draw { .. }))
                .expect("the actual Throne draw rider must parse");
            assert_eq!(
                ability.activation_mana_payment_restriction,
                Some(ActivationManaPaymentRestriction::OnlySourceChosenColor)
            );
            let rows = activatable_mana_source_selections(r.state(), P0);
            assert!(
                rows.iter().any(|row| row.source.object_id == boilerworks),
                "the off-color multicolor source must reach source selection"
            );
            let result = r.act(GameAction::ActivateAbility {
                source_id: throne,
                ability_index: index,
                payment_mode: CastPaymentMode::Auto,
            });
            assert!(
                !r.state().objects[&boilerworks].tapped,
                "off-color mana cannot pay the chosen-green activation"
            );
            if let Some(lotus) = lotus {
                result.expect("Lotus's green row pays despite earlier off-color rows");
                assert!(r.state().objects[&lotus].tapped);
                assert!(r.state().objects[&throne].tapped);
                assert_eq!(r.state().stack.len(), 1);
                let expected_surplus = if cost == "{Z}" { 2 } else { 0 };
                assert_eq!(r.state().players[0].mana_pool.total(), expected_surplus);
                assert!(r.state().players[0]
                    .mana_pool
                    .units()
                    .all(|unit| unit.source_id == lotus && unit.color == ManaType::Green));
                r.pass_both_players();
                assert_eq!(r.state().players[0].hand.len(), 2);
                assert_complete(&r, expected_surplus);
            } else {
                assert!(result.is_err(), "off-color sources alone must fail payment");
                assert!(!r.state().objects[&throne].tapped);
                assert!(r.state().players[0].hand.is_empty());
                assert_complete(&r, 0);
            }
        }
    }
}

// CR 205.4g: preserve the printed Snow supertype through layer rebuilds.
fn snow_forest(s: &mut GameScenario) -> ObjectId {
    s.add_land_from_oracle(P0, "Snow-Covered Forest", "({T}: Add {G}.)")
        .as_snow()
        .with_subtypes(vec!["Forest"])
        .id()
}

// The Oracle constructor supplies Land and Forest; complete the printed Basic
// supertype as well, so this fixture follows the real source preference tier.
fn finish_snow_forest(r: &mut GameRunner, forest: ObjectId) {
    let object = r.state_mut().objects.get_mut(&forest).unwrap();
    object.base_card_types.supertypes.push(Supertype::Basic);
    object.card_types.supertypes.push(Supertype::Basic);
}

#[test]
fn counted_druid_generic_surplus_does_not_replace_snow_payment() {
    // CR 107.4h: Sphere makes the actual Astrolabe cost {1}{S}; the Druid's
    // three ordinary units can pay the generic unit, never the snow symbol.
    let mut s = board();
    let druid = s.add_creature_from_oracle(P0, "Incubation Druid", 0, 2,
        "{T}: Add one mana of any type that a land you control could produce. If this creature has a +1/+1 counter on it, add three mana of that type instead.\n{3}{G}{G}: Adapt 3. (If this creature has no +1/+1 counters on it, put three +1/+1 counters on it.)")
        .with_plus_counters(1).with_subtypes(vec!["Elf", "Druid"]).id();
    let wastes = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
    let forest = snow_forest(&mut s);
    s.add_artifact_from_oracle(P0, "Sphere of Resistance", "Spells cost {1} more to cast.");
    let astrolabe = s.add_artifact_to_hand_from_oracle(P0, "Arcum's Astrolabe",
        "({S} can be paid with one mana from a snow source.)\nWhen this artifact enters, draw a card.\n{1}, {T}: Add one mana of any color.")
        .as_snow().with_mana_cost(parse_mtgjson_mana_cost("{S}")).id();
    let mut r = s.build();
    finish_snow_forest(&mut r, forest);
    r.state_mut().objects.get_mut(&wastes).unwrap().tapped = true;
    // Tapped Wastes still supplies the Druid's legal colorless production type.
    assert!(activatable_mana_source_selections(r.state(), P0)
        .iter()
        .any(|selection| selection.source.object_id == druid));
    r.cast(astrolabe)
        .resolve()
        .assert_zone(&[astrolabe], Zone::Battlefield);
    assert!(r.state().objects[&druid].tapped);
    assert!(r.state().objects[&forest].tapped);
    assert!(r.state().objects[&wastes].tapped);
    assert_complete(&r, 2);
    assert!(r.state().players[0]
        .mana_pool
        .units()
        .all(|unit| unit.supertype != Some(engine::types::mana::ManaSupertype::Snow)));
}

#[test]
fn snow_land_and_nonsnow_aura_reserve_distinct_units() {
    // CR 106.3 + CR 107.4h: Wild Growth's G pays G; the land's G pays S.
    // Both are green, but only one came from a snow source.
    for (float_first, life_permission, use_birds) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (false, false, true),
    ] {
        let mut s = board();
        let forest = snow_forest(&mut s);
        let (growth, birds) = if use_birds {
            let birds = s
                .add_creature_from_oracle(
                    P0,
                    "Birds of Paradise",
                    0,
                    1,
                    "Flying\n{T}: Add one mana of any color.",
                )
                .with_subtypes(vec!["Bird"])
                .id();
            (None, Some(birds))
        } else {
            let growth = s.add_enchantment_from_oracle(P0, "Wild Growth",
                "Enchant land\nWhenever enchanted land is tapped for mana, its controller adds an additional {G}.")
                .with_subtypes(vec!["Aura"]).id();
            (Some(growth), None)
        };
        s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.");
        let snow_swamp = if life_permission {
            s.add_creature_from_oracle(P0, "K'rrik, Son of Yawgmoth", 2, 2,
                "({B/P} can be paid with either {B} or 2 life.)\nLifelink\nFor each {B} in a cost, you may pay 2 life rather than pay that mana.\nWhenever you cast a black spell, put a +1/+1 counter on K'rrik.")
                .as_legendary().with_subtypes(vec!["Phyrexian", "Horror", "Minion"]);
            Some(
                s.add_land_from_oracle(P0, "Snow-Covered Swamp", "({T}: Add {B}.)")
                    .as_snow()
                    .with_subtypes(vec!["Swamp"])
                    .id(),
            )
        } else {
            s.add_basic_land(P0, ManaColor::Black);
            None
        };
        for color in [ManaColor::White, ManaColor::Blue, ManaColor::Red] {
            s.add_basic_land(P0, color);
        }
        let wurm = s.add_creature_to_hand_from_oracle(P0, "Wowzer, the Aspirational", 10, 10,
        "Whenever Wowzer, the Aspirational attacks, if you have an {E}, control a Blood, a Clue, a Food, a Map, a Powerstone, and a Treasure, are the monarch, and have the city's blessing and the initiative, you win the game.")
        .as_legendary().as_snow().with_subtypes(vec!["Wurm"])
        .with_mana_cost(parse_mtgjson_mana_cost("{C}{W}{U}{B}{R}{G}{S}")).id();
        let mut r = s.build();
        finish_snow_forest(&mut r, forest);
        if let Some(swamp) = snow_swamp {
            finish_snow_forest(&mut r, swamp);
        }
        if let Some(growth) = growth {
            engine::game::effects::attach::attach_to(r.state_mut(), growth, forest);
        }
        evaluate_layers(r.state_mut());
        if float_first {
            let (_, _, grouped) = legal_actions_full(r.state());
            let action = grouped[&forest]
                .iter()
                .find(|action| {
                    matches!(action, GameAction::TapLandForMana { selection }
                if selection.mana_type == ManaType::Green)
                })
                .expect("snow Forest exposes its green mana action")
                .clone();
            r.act(action).unwrap();
            assert_complete(&r, 2);
            assert_eq!(
                r.state().players[0]
                    .mana_pool
                    .units()
                    .filter(|unit| unit.supertype == Some(engine::types::mana::ManaSupertype::Snow))
                    .count(),
                1,
                "the existing pool has one snow and one ordinary green unit"
            );
        }
        if life_permission {
            let card_id = r.state().objects[&wurm].card_id;
            r.act(GameAction::CastSpell {
                object_id: wurm,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("announce Wowzer with K'rrik's live permission");
            let WaitingFor::PhyrexianPayment { shards, .. } = &r.state().waiting_for else {
                panic!("K'rrik must offer the deferred black payment");
            };
            assert_eq!(shards.len(), 1);
            assert_eq!(
                shards[0].shard_index, 3,
                "the printed black index survives deferral"
            );
            assert_eq!(
                shards[0].options,
                ShardOptions::ManaOrLife,
                "the snow preview preserves black by paying G with Wild Growth and S with Forest"
            );
            r.act(GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayMana],
            })
            .expect("pay black mana after the snow requirement");
            r.advance_until_stack_empty();
            assert_eq!(r.state().objects[&wurm].zone, Zone::Battlefield);
        } else {
            r.cast(wurm)
                .resolve()
                .assert_zone(&[wurm], Zone::Battlefield);
        }
        assert!(r.state().objects[&forest].tapped);
        if let Some(birds) = birds {
            assert!(
                r.state().objects[&birds].tapped,
                "ordinary green from the multicolor source must pay G while Forest pays S"
            );
        }
        assert_complete(&r, 0);
    }
}

#[test]
fn snow_pool_payment_preserves_strict_and_source_constrained_demand() {
    // Supplemental pool invariants, not invented printed cards: Wowzer covers
    // mixed colored/Snow payment above; Experiment Five's printed {1}{Z}
    // activation is the real source-constrained symbol, without a Snow symbol.
    // CR 601.2h + CR 107.4h: USU, SZ and ZUS must admit complete payment.
    let snow_blue = engine::types::mana::ManaUnit {
        supertype: Some(ManaSupertype::Snow),
        ..snow_regression_unit(ManaType::Blue, 1)
    };
    for (cost, units, expected_pips) in [
        (
            "{U}{S}{U}",
            vec![
                snow_blue.clone(),
                snow_regression_unit(ManaType::Blue, 2),
                engine::types::mana::ManaUnit {
                    supertype: Some(ManaSupertype::Snow),
                    ..snow_regression_unit(ManaType::Green, 3)
                },
            ],
            vec![2, 1, 3],
        ),
        (
            "{S}{Z}",
            vec![
                engine::types::mana::ManaUnit {
                    source_could_produce_two_or_more_colors: true,
                    ..snow_blue
                },
                engine::types::mana::ManaUnit {
                    source_could_produce_two_or_more_colors: true,
                    ..snow_regression_unit(ManaType::Blue, 2)
                },
            ],
            vec![2, 1],
        ),
        (
            "{Z}{U}{S}",
            vec![
                engine::types::mana::ManaUnit {
                    supertype: Some(ManaSupertype::Snow),
                    source_could_produce_two_or_more_colors: true,
                    ..snow_regression_unit(ManaType::Green, 1)
                },
                engine::types::mana::ManaUnit {
                    source_could_produce_two_or_more_colors: true,
                    ..snow_regression_unit(ManaType::Blue, 2)
                },
                engine::types::mana::ManaUnit {
                    supertype: Some(ManaSupertype::Snow),
                    ..snow_regression_unit(ManaType::Green, 3)
                },
            ],
            vec![1, 2, 3],
        ),
    ] {
        let mut pool = ManaPool::from_units(units);
        let cost = parse_mtgjson_mana_cost(cost);
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            CostPermissionContext::default()
        ));
        let (spent, life) = pay_from_pool(&mut pool, &cost).unwrap();
        assert!(life.is_empty());
        assert_eq!(pool.total(), 0);
        assert_eq!(
            spent.iter().map(|unit| unit.pip_id.0).collect::<Vec<_>>(),
            expected_pips
        );
    }
}

#[test]
fn snow_pool_payment_preserves_permitted_off_color_mana() {
    // These are supplemental pool shapes, not printed cards. Mycosynth Lattice
    // plus printed Wowzer exercises the permission through public casting below;
    // Experiment Five establishes Z's real source-quality requirement.
    let snow = |color, id, multicolor| ManaUnit {
        supertype: Some(ManaSupertype::Snow),
        source_could_produce_two_or_more_colors: multicolor,
        ..snow_regression_unit(color, id)
    };
    for permission in [
        ManaSpendPermission::AnyColor,
        ManaSpendPermission::AnyTypeOrColor,
    ] {
        for (cost, units, expected) in [
            (
                "{S}{U}",
                vec![
                    snow(ManaType::Blue, 1, false),
                    snow_regression_unit(ManaType::Red, 2),
                ],
                vec![2, 1],
            ),
            // Both remaining quality counts individually stay at one if U
            // consumes pip 1, but a single dual-quality pip cannot pay Z AND S.
            (
                "{U}{Z}{S}",
                vec![
                    snow(ManaType::Blue, 1, true),
                    snow(ManaType::Green, 2, true),
                    snow_regression_unit(ManaType::Red, 3),
                ],
                vec![3, 1, 2],
            ),
        ] {
            let cost = parse_mtgjson_mana_cost(cost);
            let mut pool = ManaPool::from_units(units);
            assert!(can_pay_for_spell(
                &pool,
                &cost,
                None,
                CostPermissionContext {
                    mana_spend_permission: Some(permission),
                    ..Default::default()
                }
            ));
            let (spent, life) = pay_cost_with_demand_and_choices(
                &mut pool,
                &cost,
                None,
                None,
                Some(permission),
                None,
                LifePaymentColors::EMPTY,
                &[],
            )
            .unwrap();
            assert_eq!(
                spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
                expected
            );
            assert!(life.is_empty());
            assert!(pool.is_empty());
        }
    }
    // CR 106.6: an otherwise useful off-color unit remains unusable under
    // its attached restriction; rejecting the payment must remain atomic.
    let cost = parse_mtgjson_mana_cost("{S}{U}");
    for permission in [None, Some(ManaSpendPermission::AnyColor)] {
        let mut pool = ManaPool::from_units(vec![
            snow(ManaType::Blue, 1, false),
            ManaUnit {
                restrictions: vec![ManaRestriction::OnlyForActivation],
                ..snow_regression_unit(ManaType::Red, 2)
            },
        ]);
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            Some(&PaymentContext::Effect),
            CostPermissionContext {
                mana_spend_permission: permission,
                ..Default::default()
            }
        ));
        assert!(pay_cost_with_demand_and_choices(
            &mut pool,
            &cost,
            None,
            Some(&PaymentContext::Effect),
            permission,
            None,
            LifePaymentColors::EMPTY,
            &[]
        )
        .is_err());
        assert_eq!(pool.total(), 2);
    }
    // The spend-as permission cannot erase a colorless or Snow requirement.
    for (permission, payable) in [
        (ManaSpendPermission::AnyColor, false),
        (ManaSpendPermission::AnyTypeOrColor, true),
    ] {
        let pool = ManaPool::from_units(vec![
            snow(ManaType::Colorless, 1, false),
            snow_regression_unit(ManaType::Red, 2),
        ]);
        assert_eq!(
            can_pay_for_spell(
                &pool,
                &parse_mtgjson_mana_cost("{C}{S}"),
                None,
                CostPermissionContext {
                    mana_spend_permission: Some(permission),
                    ..Default::default()
                }
            ),
            payable
        );
    }

    // CR 118.3a: an eligible explicit pin still wins. This pool has spare snow,
    // so honoring that pin completes payment rather than stranding S.
    let mut pool = ManaPool::from_units(vec![
        snow(ManaType::Blue, 1, false),
        snow_regression_unit(ManaType::Blue, 2),
        snow(ManaType::Green, 3, false),
    ]);
    let (spent, _) = pay_cost_with_demand_and_choices(
        &mut pool,
        &cost,
        None,
        None,
        None,
        None,
        LifePaymentColors::EMPTY,
        &[ManaPipId(1)],
    )
    .unwrap();
    assert_eq!(
        spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(pool.units().next().unwrap().pip_id, ManaPipId(2));
}

#[test]
fn snow_pool_payment_preserves_later_z_mana() {
    // Supplemental U/Z/S permutations, not printed-card claims. Wowzer supplies
    // mixed colored/Snow requirements; Experiment Five supplies printed Z.
    let snow = |color, id, multicolor| ManaUnit {
        supertype: Some(ManaSupertype::Snow),
        source_could_produce_two_or_more_colors: multicolor,
        ..snow_regression_unit(color, id)
    };
    let z = |color, id| ManaUnit {
        source_could_produce_two_or_more_colors: true,
        ..snow_regression_unit(color, id)
    };
    for cost in ["{U}{Z}{S}", "{S}{U}{Z}"] {
        let cost = parse_mtgjson_mana_cost(cost);
        let mut pool = ManaPool::from_units(vec![
            snow(ManaType::Blue, 1, false),
            z(ManaType::Blue, 2),
            snow(ManaType::Green, 3, false),
        ]);
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            CostPermissionContext::default()
        ));
        let (spent, life) = pay_cost_with_demand_and_choices(
            &mut pool,
            &cost,
            None,
            None,
            None,
            None,
            LifePaymentColors::EMPTY,
            &[],
        )
        .unwrap();
        assert_eq!(
            spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(life.is_empty());
        assert!(pool.is_empty());
    }

    // Restricted quality-bearing decoys cannot make the sole usable Z unit
    // appear spare. The matching unrestricted unit is already covered above.
    let mut pool = ManaPool::from_units(vec![
        snow(ManaType::Blue, 1, false),
        z(ManaType::Blue, 2),
        snow(ManaType::Green, 3, false),
        ManaUnit {
            restrictions: vec![ManaRestriction::OnlyForActivation],
            ..z(ManaType::Red, 4)
        },
    ]);
    let (spent, _) = pay_cost_with_demand_and_choices(
        &mut pool,
        &parse_mtgjson_mana_cost("{U}{Z}{S}"),
        None,
        Some(&PaymentContext::Effect),
        None,
        None,
        LifePaymentColors::EMPTY,
        &[],
    )
    .unwrap();
    assert_eq!(
        spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(pool.units().next().unwrap().pip_id, ManaPipId(4));

    // With no Snow requirement the old non-Z preference remains unchanged.
    let mut pool = ManaPool::from_units(vec![z(ManaType::Blue, 1), snow(ManaType::Blue, 2, false)]);
    let (spent, _) = pay_cost_with_demand_and_choices(
        &mut pool,
        &parse_mtgjson_mana_cost("{U}"),
        None,
        None,
        None,
        None,
        LifePaymentColors::EMPTY,
        &[],
    )
    .unwrap();
    assert_eq!(spent[0].pip_id, ManaPipId(2));

    // The preview must preserve the same quality allocation before offering
    // a later Phyrexian mana option, using its original printed index.
    let cost = parse_mtgjson_mana_cost("{U}{Z}{S}{B/P}");
    let mut pool = ManaPool::from_units(vec![
        snow(ManaType::Blue, 1, false),
        z(ManaType::Blue, 2),
        snow(ManaType::Green, 3, false),
        snow_regression_unit(ManaType::Black, 4),
    ]);
    let preview = compute_phyrexian_shards(
        &pool,
        &cost,
        None,
        CostPermissionContext {
            max_life: 1,
            ..Default::default()
        },
    );
    assert_eq!(preview.len(), 1);
    assert_eq!(preview[0].shard_index, 3);
    assert_eq!(preview[0].options, ShardOptions::ManaOrLife);
    let (spent, life) = pay_cost_with_demand_and_choices(
        &mut pool,
        &cost,
        None,
        None,
        None,
        Some(&[ShardChoice::PayMana]),
        LifePaymentColors::EMPTY,
        &[],
    )
    .unwrap();
    assert_eq!(
        spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(life.is_empty());
    assert!(pool.is_empty());
}

#[test]
fn snow_pool_z_fallback_preserves_snow_before_phyrexian_life() {
    // Supplemental combined shape, not a printed-card claim: Experiment Five
    // supplies Z, Wowzer supplies Snow, and Gitaxian Probe supplies U/P.
    // No printed card combines these requirements. CR 107.4f + CR 107.4h:
    // spend ordinary U on Z, snow G on S, and two life on U/P.
    let pool = ManaPool::from_units(vec![
        ManaUnit {
            supertype: Some(ManaSupertype::Snow),
            source_could_produce_two_or_more_colors: true,
            ..snow_regression_unit(ManaType::Green, 1)
        },
        ManaUnit {
            source_could_produce_two_or_more_colors: true,
            ..snow_regression_unit(ManaType::Blue, 2)
        },
    ]);
    for context in [None, Some(PaymentContext::Effect)] {
        for (text, phyrexian_index) in [("{Z}{U/P}{S}", 1), ("{S}{Z}{U/P}", 2), ("{U/P}{S}{Z}", 0)]
        {
            let cost = parse_mtgjson_mana_cost(text);
            let permissions = CostPermissionContext {
                max_life: 1,
                ..Default::default()
            };
            assert!(can_pay_for_spell(
                &pool,
                &cost,
                context.as_ref(),
                permissions
            ));
            // CR 118.3: the same pool cannot pay the cost without two life.
            assert!(!can_pay_for_spell(
                &pool,
                &cost,
                context.as_ref(),
                CostPermissionContext::default(),
            ));
            let preview = compute_phyrexian_shards(&pool, &cost, context.as_ref(), permissions);
            assert_eq!(preview.len(), 1);
            assert_eq!(preview[0].shard_index, phyrexian_index);
            assert_eq!(preview[0].options, ShardOptions::LifeOnly);
            let mut mana_only_pool = pool.clone();
            assert!(pay_cost_with_demand_and_choices(
                &mut mana_only_pool,
                &cost,
                None,
                context.as_ref(),
                None,
                Some(&[ShardChoice::PayMana]),
                LifePaymentColors::EMPTY,
                &[],
            )
            .is_err());
            assert_eq!(
                mana_only_pool, pool,
                "forced mana cannot consume a partial payment"
            );
            for choices in [None, Some([ShardChoice::PayLife])] {
                let mut payment_pool = pool.clone();
                let (spent, life) = pay_cost_with_demand_and_choices(
                    &mut payment_pool,
                    &cost,
                    None,
                    context.as_ref(),
                    None,
                    choices.as_ref().map(|choices| choices.as_slice()),
                    LifePaymentColors::EMPTY,
                    &[],
                )
                .unwrap();
                assert_eq!(
                    spent.iter().map(|unit| unit.pip_id.0).collect::<Vec<_>>(),
                    vec![2, 1]
                );
                assert_eq!(life.len(), 1);
                assert_eq!(life[0].amount, 2);
                assert!(payment_pool.is_empty());
            }
        }

        // With no Snow cost, retain the first Z source and pay U/P with mana.
        // An explicit pin still selects that source when spare Snow makes the
        // full cost payable. Both controls preserve the original source order.
        for (text, pins, extra_snow, expected_pips) in [
            ("{Z}{U/P}", vec![], false, vec![1, 2]),
            ("{Z}{U/P}{S}", vec![ManaPipId(1)], true, vec![1, 3, 2]),
        ] {
            let mut payment_pool = pool.clone();
            if extra_snow {
                payment_pool.add(ManaUnit {
                    supertype: Some(ManaSupertype::Snow),
                    ..snow_regression_unit(ManaType::Red, 3)
                });
            }
            let (spent, life) = pay_cost_with_demand_and_choices(
                &mut payment_pool,
                &parse_mtgjson_mana_cost(text),
                None,
                context.as_ref(),
                None,
                None,
                LifePaymentColors::EMPTY,
                &pins,
            )
            .unwrap();
            assert_eq!(
                spent.iter().map(|unit| unit.pip_id.0).collect::<Vec<_>>(),
                expected_pips
            );
            assert!(life.is_empty());
            assert!(payment_pool.is_empty());
        }
    }
}

// Supplemental combinations of Experiment Five's Z, Ulalek's C/color,
// Edge of the Divinity's W/B, Arcum's Astrolabe's S and Gut Shot's R/P.
// No printed card is claimed to have these complete costs. The real land
// Oracle below supplies every unit through public actions (CR 601.2g).
fn float_hybrid_reserve_sources(
    cost: &str,
    sources: &[(&str, ManaType)],
    life_after_activations: i32,
) -> (GameRunner, ObjectId) {
    float_hybrid_reserve_sources_with_extra(cost, sources, life_after_activations, None)
}

fn float_hybrid_reserve_sources_with_extra(
    cost: &str,
    sources: &[(&str, ManaType)],
    life_after_activations: i32,
    extra_land: Option<ManaColor>,
) -> (GameRunner, ObjectId) {
    let mut s = board();
    let mut ids = Vec::new();
    for &(name, color) in sources {
        let (text, subtypes, snow, multicolor) = match name {
            "Highland Forest" => (
                "({T}: Add {R} or {G}.)\nThis land enters tapped.",
                vec!["Mountain", "Forest"],
                true,
                true,
            ),
            "Adarkar Wastes" => (
                "{T}: Add {C}.\n{T}: Add {W} or {U}. This land deals 1 damage to you.",
                vec![],
                false,
                true,
            ),
            "Caves of Koilos" => (
                "{T}: Add {C}.\n{T}: Add {W} or {B}. This land deals 1 damage to you.",
                vec![],
                false,
                true,
            ),
            "Island" => ("({T}: Add {U}.)", vec!["Island"], false, false),
            "Swamp" => ("({T}: Add {B}.)", vec!["Swamp"], false, false),
            "Plains" => ("({T}: Add {W}.)", vec!["Plains"], false, false),
            "Mountain" | "Snow-Covered Mountain" => (
                "({T}: Add {R}.)",
                vec!["Mountain"],
                name == "Snow-Covered Mountain",
                false,
            ),
            _ => panic!("unlisted real mana source {name}"),
        };
        let mut source = s.add_land_from_oracle(P0, name, text);
        source.with_subtypes(subtypes);
        let id = if snow {
            source.as_snow().id()
        } else {
            source.id()
        };
        let damage = i32::from(
            matches!(name, "Adarkar Wastes" | "Caves of Koilos") && color != ManaType::Colorless,
        );
        ids.push((id, color, snow, multicolor, damage));
    }
    if let Some(color) = extra_land {
        s.add_basic_land(P0, color);
    }
    let creature = s
        .add_creature_to_hand_from_oracle(P0, "Supplemental hybrid/Z/S payment", 1, 1, "")
        .with_mana_cost(parse_mtgjson_mana_cost(cost))
        .id();
    let mut r = s.build();
    let activation_damage: i32 = ids.iter().map(|(_, _, _, _, damage)| damage).sum();
    r.state_mut().players[0].life = life_after_activations + activation_damage;
    for ((id, _, _, _, _), (name, _)) in ids.iter().zip(sources) {
        // All lands have been controlled since an earlier turn. Finish the
        // printed Basic supertype, then let the ordinary layer pass run.
        r.state_mut().objects.get_mut(id).unwrap().tapped = false;
        if !matches!(
            *name,
            "Highland Forest" | "Adarkar Wastes" | "Caves of Koilos"
        ) {
            finish_snow_forest(&mut r, *id);
        }
    }
    evaluate_layers(r.state_mut());
    for &(source, color, _, _, damage) in &ids {
        let life_before = r.state().players[0].life;
        let (_, _, grouped) = legal_actions_full(r.state());
        let action = grouped[&source]
            .iter()
            .find(|action| {
                matches!(action,
            GameAction::TapLandForMana { selection } if selection.mana_type == color)
            })
            .expect("real source offers its required mana action")
            .clone();
        r.act(action).unwrap();
        assert_eq!(
            r.state().players[0].life,
            life_before - damage,
            "source damage is separate from Phyrexian life"
        );
    }
    let units: Vec<_> = r.state().players[0].mana_pool.units().collect();
    assert_eq!(units.len(), ids.len());
    for (unit, &(source, color, snow, multicolor, _)) in units.iter().zip(&ids) {
        assert_eq!(unit.source_id, source);
        assert_eq!(unit.color, color);
        assert_eq!(unit.supertype == Some(ManaSupertype::Snow), snow);
        assert_eq!(unit.source_could_produce_two_or_more_colors, multicolor);
        assert!(r.state().objects[&source].tapped);
    }
    assert_eq!(r.state().players[0].life, life_after_activations);
    assert_complete(&r, sources.len());
    (r, creature)
}

fn cast_floated_hybrid_reserve(
    mut r: GameRunner,
    creature: ObjectId,
    succeeds: bool,
    phyrexian: Option<(usize, ShardOptions, ShardChoice)>,
    life_paid: i32,
) {
    let pool = r.state().players[0].mana_pool.clone();
    let pips: Vec<_> = pool.units().map(|unit| unit.pip_id).collect();
    let life = r.state().players[0].life;
    let result = r.act(GameAction::CastSpell {
        object_id: creature,
        card_id: r.state().objects[&creature].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    if !succeeds {
        // CR 601.2h: rejection must leave the entire payment uncommitted.
        assert!(result.is_err());
        assert_eq!(r.state().objects[&creature].zone, Zone::Hand);
        assert_eq!(r.state().players[0].life, life);
        assert_eq!(r.state().players[0].mana_pool, pool);
        assert_eq!(
            r.state().players[0]
                .mana_pool
                .units()
                .map(|unit| unit.pip_id)
                .collect::<Vec<_>>(),
            pips
        );
        assert_complete(&r, pool.total());
        return;
    }
    result.expect("all-floating hybrid alternative must pay the complete cost");
    if let Some((index, options, choice)) = phyrexian {
        let WaitingFor::PhyrexianPayment {
            player,
            spell_object,
            shards,
        } = &r.state().waiting_for
        else {
            panic!("expected the public Phyrexian choice before committing life");
        };
        assert_eq!(*player, P0);
        assert_eq!(*spell_object, creature);
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].shard_index, index);
        assert_eq!(shards[0].options, options);
        r.act(GameAction::SubmitPhyrexianChoices {
            choices: vec![choice],
        })
        .unwrap();
    }
    assert_eq!(r.state().stack.len(), 1);
    assert!(r.state().players[0].mana_pool.is_empty());
    assert_eq!(r.state().players[0].life, life - life_paid);
    r.advance_until_stack_empty();
    assert_eq!(r.state().objects[&creature].zone, Zone::Battlefield);
    assert_complete(&r, 0);
}

#[test]
fn snow_pool_z_hybrid_fully_floated_public_casts() {
    // H1/H2: CR 107.4e + CR 107.4h. Both alternative halves are available,
    // but each Snow/Z/hybrid payment must consume a distinct physical unit.
    for (cost, sources) in [
        (
            "{Z}{C/U}{S}",
            vec![
                ("Highland Forest", ManaType::Green),
                ("Adarkar Wastes", ManaType::Colorless),
                ("Island", ManaType::Blue),
            ],
        ),
        (
            "{Z}{W/B}{S}",
            vec![
                ("Highland Forest", ManaType::Green),
                ("Caves of Koilos", ManaType::White),
                ("Swamp", ManaType::Black),
            ],
        ),
        (
            "{Z}{Z}{C/U}{W/B}{S}",
            vec![
                ("Highland Forest", ManaType::Green),
                ("Adarkar Wastes", ManaType::Colorless),
                ("Caves of Koilos", ManaType::White),
                ("Island", ManaType::Blue),
                ("Swamp", ManaType::Black),
            ],
        ),
    ] {
        let (r, creature) = float_hybrid_reserve_sources(cost, &sources, 20);
        cast_floated_hybrid_reserve(r, creature, true, None, 0);
        // Removing the ordinary alternative cannot be rescued by an untapped land.
        let (r, creature) = float_hybrid_reserve_sources(cost, &sources[..sources.len() - 1], 20);
        cast_floated_hybrid_reserve(r, creature, false, None, 0);
    }
    let (r, creature) = float_hybrid_reserve_sources(
        "{Z}{Z}{C/U}{W/B}{S}",
        &[
            ("Highland Forest", ManaType::Green),
            ("Adarkar Wastes", ManaType::Colorless),
            ("Plains", ManaType::White),
            ("Island", ManaType::Blue),
            ("Swamp", ManaType::Black),
        ],
        20,
    );
    cast_floated_hybrid_reserve(r, creature, false, None, 0);
}

#[test]
fn snow_pool_z_hybrid_phyrexian_public_mana_life_and_budget() {
    // H3/H4: CR 107.4f. At one life, the full-pool check must choose the
    // zero-life route even when the old conservative tier selected snow G.
    for (cost, index) in [("{Z}{C/U}{S}{R/P}", 3), ("{R/P}{Z}{C/U}{S}", 0)] {
        for red_source in [None, Some("Mountain"), Some("Snow-Covered Mountain")] {
            for life in [1, 3] {
                let mut sources = vec![
                    ("Highland Forest", ManaType::Green),
                    ("Adarkar Wastes", ManaType::Colorless),
                    ("Island", ManaType::Blue),
                ];
                if let Some(name) = red_source {
                    sources.push((name, ManaType::Red));
                }
                let (r, creature) = float_hybrid_reserve_sources(cost, &sources, life);
                let succeeds = red_source.is_some() || life == 3;
                let prompt = (life == 3).then_some((
                    index,
                    if red_source.is_some() {
                        ShardOptions::ManaOrLife
                    } else {
                        ShardOptions::LifeOnly
                    },
                    if red_source.is_some() {
                        ShardChoice::PayMana
                    } else {
                        ShardChoice::PayLife
                    },
                ));
                cast_floated_hybrid_reserve(
                    r,
                    creature,
                    succeeds,
                    prompt,
                    if red_source.is_none() && succeeds {
                        2
                    } else {
                        0
                    },
                );
            }
        }
    }
}

fn hybrid_reserve_unit(color: ManaType, id: u64, snow: bool, multicolor: bool) -> ManaUnit {
    ManaUnit {
        supertype: snow.then_some(ManaSupertype::Snow),
        source_could_produce_two_or_more_colors: multicolor,
        ..snow_regression_unit(color, id)
    }
}

#[test]
fn snow_pool_z_hybrid_all_pairs_and_recursive_units() {
    // H1/H2/H7: cover every printed two-way color pairing and C/color axis.
    // Repeated Z controls have independently sufficient units, not duplicated
    // pip identities. Runtime growth is measured by the parent, not a test timer.
    let colors = [
        ("W", ManaType::White),
        ("U", ManaType::Blue),
        ("B", ManaType::Black),
        ("R", ManaType::Red),
        ("G", ManaType::Green),
    ];
    for (i, &(a_text, a)) in colors.iter().enumerate() {
        for &(b_text, b) in &colors[i + 1..] {
            let (first, second) = match (a_text, b_text) {
                ("W", "R") | ("W", "G") | ("U", "G") => (b_text, a_text),
                _ => (a_text, b_text),
            };
            let cost = parse_mtgjson_mana_cost(&format!("{{Z}}{{{first}/{second}}}{{S}}"));
            assert!(matches!(&cost, ManaCost::Cost { shards, generic: 0 } if shards.len() == 3));
            for (z, ordinary) in [(a, b), (b, a)] {
                let pool = ManaPool::from_units(vec![
                    hybrid_reserve_unit(ManaType::Colorless, 1, true, true),
                    hybrid_reserve_unit(z, 2, false, true),
                    hybrid_reserve_unit(ordinary, 3, false, false),
                ]);
                let mut payment = pool.clone();
                let (spent, life) = pay_cost_with_demand_and_choices(
                    &mut payment,
                    &cost,
                    None,
                    None,
                    None,
                    None,
                    LifePaymentColors::EMPTY,
                    &[],
                )
                .unwrap();
                assert_eq!(
                    spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
                    [2, 3, 1]
                );
                assert!(life.is_empty());
                assert!(payment.is_empty());
            }
        }
        for count in [1, 2, 4, 8] {
            let mut units = vec![hybrid_reserve_unit(ManaType::Green, 1, true, true)];
            units.extend(
                (0..count).map(|n| hybrid_reserve_unit(ManaType::Colorless, 2 + n, false, true)),
            );
            units.extend((0..count).map(|n| hybrid_reserve_unit(a, 20 + n, false, false)));
            let mut pool = ManaPool::from_units(units);
            let cost = format!(
                "{}{}{{S}}",
                "{Z}".repeat(count as usize),
                format!("{{C/{a_text}}}").repeat(count as usize)
            );
            let (spent, life) = pay_cost_with_demand_and_choices(
                &mut pool,
                &parse_mtgjson_mana_cost(&cost),
                None,
                None,
                None,
                None,
                LifePaymentColors::EMPTY,
                &[],
            )
            .unwrap();
            assert!(spent[..count as usize].iter().all(
                |u| u.color == ManaType::Colorless && u.supertype != Some(ManaSupertype::Snow)
            ));
            assert!(life.is_empty());
            assert!(pool.is_empty());
        }
    }
}

#[test]
fn snow_pool_z_hybrid_unpayable_and_forced_life_suffixes() {
    // Supplemental repeated Experiment Five {Z} / Astrolabe {S} / Gut Shot
    // {R/P} composition: CR 601.2h and 107.4f. Missing mandatory mana must
    // reject atomically, and unavoidable life must not invite permutation search.
    for count in [4, 6, 8] {
        for snow_color in [ManaType::Green, ManaType::Red] {
            let mut units: Vec<_> = (0..count)
                .map(|n| hybrid_reserve_unit(ManaType::Colorless, n + 1, false, true))
                .collect();
            units.push(hybrid_reserve_unit(snow_color, 20, true, true));
            let funded = ManaPool::from_units(units);
            let z = "{Z}".repeat(count as usize);
            for choices in [None, Some([ShardChoice::PayLife])] {
                let mut pool = funded.clone();
                let (spent, life) = pay_cost_with_demand_and_choices(
                    &mut pool,
                    &parse_mtgjson_mana_cost(&format!("{z}{{S}}{{R/P}}")),
                    None,
                    None,
                    None,
                    choices.as_ref().map(|v| v.as_slice()),
                    LifePaymentColors::EMPTY,
                    &[],
                )
                .unwrap();
                assert_eq!(spent.len(), count as usize + 1);
                assert_eq!(life.len(), 1);
                assert!(pool.is_empty());
            }
            for suffix in ["{U}{S}", "{2}{S}"] {
                let mut pool = funded.clone();
                pool.add(hybrid_reserve_unit(ManaType::Red, 21, false, false));
                let before = pool.clone();
                assert!(pay_cost_with_demand_and_choices(
                    &mut pool,
                    &parse_mtgjson_mana_cost(&format!("{z}{suffix}")),
                    None,
                    None,
                    None,
                    None,
                    LifePaymentColors::EMPTY,
                    &[],
                )
                .is_err());
                assert_eq!(pool, before);
            }
        }
        for hybrid in ["{C/U}", "{W/U}"] {
            for ordinary in [ManaType::Black, ManaType::Blue] {
                let mut units: Vec<_> = (0..count)
                    .map(|n| hybrid_reserve_unit(ManaType::Red, n + 1, false, true))
                    .collect();
                units.push(hybrid_reserve_unit(ManaType::Green, 20, true, true));
                units.push(hybrid_reserve_unit(ordinary, 21, false, false));
                let mut pool = ManaPool::from_units(units);
                let before = pool.clone();
                let cost = parse_mtgjson_mana_cost(&format!(
                    "{}{hybrid}{{S}}",
                    "{Z}".repeat(count as usize)
                ));
                let result = pay_cost_with_demand_and_choices(
                    &mut pool,
                    &cost,
                    None,
                    None,
                    None,
                    None,
                    LifePaymentColors::EMPTY,
                    &[],
                );
                if ordinary == ManaType::Blue {
                    let (spent, life) = result.unwrap();
                    assert_eq!(spent.len(), count as usize + 2);
                    assert!(life.is_empty());
                    assert!(pool.is_empty());
                } else {
                    assert!(result.is_err());
                    assert_eq!(pool, before);
                }
            }
        }
    }
}

#[test]
fn snow_pool_z_hybrid_repeated_partial_phyrexian_auto_tap() {
    // Supplemental Experiment Five / Astrolabe / Gut Shot cost (CR 601.2g/h).
    // The floating pool cannot pay R/P with mana. At one life the only legal
    // completion is to retain its Z/S units and auto-tap the ordinary Mountain.
    for count in [4, 8] {
        let cost = format!("{}{{S}}{{R/P}}", "{Z}".repeat(count));
        let mut sources = vec![("Adarkar Wastes", ManaType::Colorless); count];
        sources.push(("Highland Forest", ManaType::Green));
        for extra in [ManaColor::Red, ManaColor::Blue] {
            let (r, creature) =
                float_hybrid_reserve_sources_with_extra(&cost, &sources, 1, Some(extra));
            cast_floated_hybrid_reserve(r, creature, extra == ManaColor::Red, None, 0);
        }
    }
}

#[test]
fn snow_pool_z_hybrid_life_choices_indices_and_memo() {
    // H3/H4/H6: all explicit choices keep their original raw positions, even
    // when the Phyrexian symbols precede Z and payment defers them.
    for (cost, life_colors, indices) in [
        (
            "{R/P}{Z}{C/U}{S}{B/P}",
            LifePaymentColors::EMPTY,
            vec![0, 4],
        ),
        (
            "{B}{Z}{C/U}{S}{R/P}",
            [ManaColor::Black].into_iter().collect(),
            vec![0, 4],
        ),
    ] {
        let pool = ManaPool::from_units(vec![
            hybrid_reserve_unit(ManaType::Green, 1, true, true),
            hybrid_reserve_unit(ManaType::Colorless, 2, false, true),
            hybrid_reserve_unit(ManaType::Blue, 3, false, false),
            hybrid_reserve_unit(ManaType::Red, 4, true, false),
        ]);
        let cost = parse_mtgjson_mana_cost(cost);
        let preview = compute_phyrexian_shards(
            &pool,
            &cost,
            None,
            CostPermissionContext {
                max_life: 1,
                life_colors,
                ..Default::default()
            },
        );
        assert_eq!(
            preview.iter().map(|s| s.shard_index).collect::<Vec<_>>(),
            indices
        );
        let choices = if life_colors == LifePaymentColors::EMPTY {
            [ShardChoice::PayMana, ShardChoice::PayLife]
        } else {
            [ShardChoice::PayLife, ShardChoice::PayMana]
        };
        let mut payment = pool.clone();
        let (spent, life) = pay_cost_with_demand_and_choices(
            &mut payment,
            &cost,
            None,
            None,
            None,
            Some(&choices),
            life_colors,
            &[],
        )
        .unwrap();
        assert_eq!(
            spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
            [2, 3, 1, 4]
        );
        assert_eq!(life.len(), 1);
        assert!(payment.is_empty());
        let mut failure = pool.clone();
        assert!(pay_cost_with_demand_and_choices(
            &mut failure,
            &cost,
            None,
            None,
            None,
            Some(&[ShardChoice::PayMana, ShardChoice::PayMana]),
            life_colors,
            &[]
        )
        .is_err());
        assert_eq!(failure, pool);
        assert_eq!(
            failure.units().map(|u| u.pip_id).collect::<Vec<_>>(),
            pool.units().map(|u| u.pip_id).collect::<Vec<_>>()
        );
    }

    for (text, index) in [
        ("{W/B}{Z}{C/U}{S}{W/B/P}", 4),
        ("{W/B/P}{W/B}{Z}{C/U}{S}", 0),
    ] {
        let mut pool = ManaPool::from_units(vec![
            hybrid_reserve_unit(ManaType::Green, 1, true, true),
            hybrid_reserve_unit(ManaType::Colorless, 2, false, true),
            hybrid_reserve_unit(ManaType::Blue, 3, false, false),
            hybrid_reserve_unit(ManaType::White, 4, false, false),
            hybrid_reserve_unit(ManaType::White, 5, false, false),
            hybrid_reserve_unit(ManaType::Black, 6, false, false),
            hybrid_reserve_unit(ManaType::Black, 7, false, false),
        ]);
        let cost = parse_mtgjson_mana_cost(text);
        let preview = compute_phyrexian_shards(
            &pool,
            &cost,
            None,
            CostPermissionContext {
                max_life: 1,
                ..Default::default()
            },
        );
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].shard_index, index);
        assert_eq!(preview[0].options, ShardOptions::ManaOrLife);
        let (spent, life) = pay_cost_with_demand_and_choices(
            &mut pool,
            &cost,
            None,
            None,
            None,
            Some(&[ShardChoice::PayMana]),
            LifePaymentColors::EMPTY,
            &[],
        )
        .unwrap();
        assert_eq!(
            spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
            [4, 2, 3, 1, 5],
            "the earlier W preference survives the Z proof although more B remains"
        );
        assert!(life.is_empty());
        assert!(pool.units().all(|u| u.color == ManaType::Black));
    }
}

#[test]
fn snow_pool_z_hybrid_keeps_executable_baseline_and_pins() {
    // H6: the tempting C/Z alternative strands strict C because the existing
    // C/U dispatch greedily consumes the other C. An abstract matching using
    // U for C/U is not an executable witness for this engine policy.
    for phyrexian in [false, true] {
        let cost = parse_mtgjson_mana_cost(if phyrexian {
            "{Z}{C/U}{C}{S}{R/P}"
        } else {
            "{Z}{C/U}{C}{S}"
        });
        let mut pool = ManaPool::from_units(vec![
            hybrid_reserve_unit(ManaType::Green, 1, true, true),
            hybrid_reserve_unit(ManaType::Colorless, 2, false, true),
            hybrid_reserve_unit(ManaType::Colorless, 3, false, false),
            hybrid_reserve_unit(ManaType::Blue, 4, false, false),
            hybrid_reserve_unit(ManaType::Red, 5, true, false),
        ]);
        let (spent, life) = pay_cost_with_demand_and_choices(
            &mut pool,
            &cost,
            None,
            None,
            None,
            None,
            LifePaymentColors::EMPTY,
            &[],
        )
        .unwrap();
        assert_eq!(
            spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
            [1, 3, 2, 5]
        );
        assert_eq!(life.len(), usize::from(phyrexian));
        assert_eq!(pool.units().map(|u| u.pip_id.0).collect::<Vec<_>>(), [4]);
    }
    let cost = parse_mtgjson_mana_cost("{Z}{C/U}{S}");
    for spare_snow in [false, true] {
        let mut units = vec![
            hybrid_reserve_unit(ManaType::Green, 1, true, true),
            hybrid_reserve_unit(ManaType::Colorless, 2, false, true),
            hybrid_reserve_unit(ManaType::Blue, 3, false, false),
        ];
        if spare_snow {
            units.push(hybrid_reserve_unit(ManaType::Red, 4, true, false));
        }
        let original = ManaPool::from_units(units);
        let mut pool = original.clone();
        let result = pay_cost_with_demand_and_choices(
            &mut pool,
            &cost,
            None,
            None,
            None,
            None,
            LifePaymentColors::EMPTY,
            &[ManaPipId(1)],
        );
        if spare_snow {
            let (spent, life) = result.unwrap();
            assert_eq!(
                spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
                [1, 2, 4]
            );
            assert!(life.is_empty());
        } else {
            let (spent, life) = result.expect("a pin is a priority hint, not a shard assignment");
            assert_eq!(
                spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
                [2, 3, 1]
            );
            assert!(life.is_empty());
            assert!(pool.is_empty());
        }
    }
}

#[test]
fn snow_pool_z_hybrid_ranks_life_after_failed_baseline_and_keeps_ties() {
    // H4: baseline snow G cannot complete; the first alternative (R/Z)
    // needs life, while the later C/Z leaves R for the Phyrexian payment.
    let mut pool = ManaPool::from_units(vec![
        hybrid_reserve_unit(ManaType::Green, 1, true, true),
        hybrid_reserve_unit(ManaType::Red, 2, false, true),
        hybrid_reserve_unit(ManaType::Colorless, 3, false, true),
        hybrid_reserve_unit(ManaType::Blue, 4, false, false),
        hybrid_reserve_unit(ManaType::Black, 5, false, false),
        hybrid_reserve_unit(ManaType::Black, 6, false, false),
    ]);
    let cost = parse_mtgjson_mana_cost("{Z}{C/U}{B/R}{S}{R/P}");
    assert!(can_pay_for_spell(
        &pool,
        &cost,
        None,
        CostPermissionContext::default()
    ));
    let (spent, life) = pay_cost_with_demand_and_choices(
        &mut pool,
        &cost,
        None,
        None,
        None,
        None,
        LifePaymentColors::EMPTY,
        &[],
    )
    .unwrap();
    assert_eq!(
        spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
        [3, 4, 6, 1, 2]
    );
    assert!(life.is_empty());
    assert_eq!(pool.units().map(|u| u.pip_id.0).collect::<Vec<_>>(), [5]);

    // A binding PayLife choice makes both complete routes cost one life
    // payment. Preserve the full old baseline, including its second tier.
    let mut pool = ManaPool::from_units(vec![
        hybrid_reserve_unit(ManaType::Green, 1, true, true),
        hybrid_reserve_unit(ManaType::Colorless, 2, false, true),
        hybrid_reserve_unit(ManaType::Blue, 3, false, false),
        hybrid_reserve_unit(ManaType::Red, 4, true, false),
    ]);
    let (spent, life) = pay_cost_with_demand_and_choices(
        &mut pool,
        &parse_mtgjson_mana_cost("{Z}{C/U}{S}{R/P}"),
        None,
        None,
        None,
        Some(&[ShardChoice::PayLife]),
        LifePaymentColors::EMPTY,
        &[],
    )
    .unwrap();
    assert_eq!(
        spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
        [1, 2, 4]
    );
    assert_eq!(life.len(), 1);
    assert_eq!(pool.units().next().unwrap().pip_id, ManaPipId(3));
}

#[test]
fn snow_pool_z_hybrid_context_generic_and_quality_controls() {
    // H7: CR 106.6. Restrict each distinct role in turn; neither quality
    // counts nor the suffix proof may treat an ineligible decoy as mana.
    let meta = SpellMeta {
        types: vec!["Creature".to_owned()],
        ..Default::default()
    };
    let contexts = [
        PaymentContext::Spell(&meta),
        PaymentContext::Activation {
            source_types: &[],
            source_subtypes: &[],
            ability_tag: None,
            mana_color_constraint: ActivationManaColorConstraint::Unrestricted,
        },
    ];
    for restricted in [None, Some(0), Some(1), Some(2)] {
        let mut units = vec![
            hybrid_reserve_unit(ManaType::Green, 1, true, true),
            hybrid_reserve_unit(ManaType::Colorless, 2, false, true),
            hybrid_reserve_unit(ManaType::Blue, 3, false, false),
        ];
        if let Some(index) = restricted {
            units[index]
                .restrictions
                .push(ManaRestriction::OnlyForActivation);
        }
        let original = ManaPool::from_units(units);
        for (index, context) in contexts.iter().enumerate() {
            let mut pool = original.clone();
            let result = pay_cost_with_demand_and_choices(
                &mut pool,
                &parse_mtgjson_mana_cost("{Z}{C/U}{S}"),
                None,
                Some(context),
                None,
                None,
                LifePaymentColors::EMPTY,
                &[],
            );
            if restricted.is_none() || index == 1 {
                let (spent, life) = result.unwrap();
                assert_eq!(
                    spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
                    [2, 3, 1]
                );
                assert!(life.is_empty());
                assert!(pool.is_empty());
            } else {
                assert!(result.is_err());
                assert_eq!(pool, original);
                assert_eq!(
                    pool.units().map(|u| u.pip_id).collect::<Vec<_>>(),
                    original.units().map(|u| u.pip_id).collect::<Vec<_>>()
                );
            }
        }
    }
    // Generic and restricted-X payment remain after the exact suffix. No
    // already-paid prefix shard may reappear as a residual requirement.
    for restricted_x in [false, true] {
        for extra in [None, Some(ManaType::White), Some(ManaType::Red)] {
            let mut units = vec![
                hybrid_reserve_unit(ManaType::Green, 1, true, true),
                hybrid_reserve_unit(ManaType::Colorless, 2, false, true),
                hybrid_reserve_unit(ManaType::Blue, 3, false, false),
            ];
            if let Some(color) = extra {
                units.push(hybrid_reserve_unit(color, 4, false, false));
            }
            let original = ManaPool::from_units(units);
            let meta = SpellMeta {
                spend_only_on_x_colors: restricted_x.then_some(vec![ManaColor::White]),
                spend_only_on_x_generic_count: u32::from(restricted_x),
                ..Default::default()
            };
            let mut pool = original.clone();
            let result = pay_cost_with_demand_and_choices(
                &mut pool,
                &parse_mtgjson_mana_cost("{1}{Z}{C/U}{S}"),
                None,
                Some(&PaymentContext::Spell(&meta)),
                None,
                None,
                LifePaymentColors::EMPTY,
                &[],
            );
            if extra.is_some() && (!restricted_x || extra == Some(ManaType::White)) {
                let (spent, life) = result.unwrap();
                assert_eq!(
                    spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
                    [2, 3, 1, 4]
                );
                assert!(life.is_empty());
                assert!(pool.is_empty());
            } else {
                assert!(result.is_err());
                assert_eq!(pool, original);
            }
        }
    }
    // CR 107.4c: AnyColor still cannot substitute blue for strict C.
    for permission in [
        ManaSpendPermission::AnyColor,
        ManaSpendPermission::AnyTypeOrColor,
    ] {
        let original = ManaPool::from_units(vec![
            hybrid_reserve_unit(ManaType::Green, 1, true, true),
            hybrid_reserve_unit(ManaType::Colorless, 2, false, true),
            hybrid_reserve_unit(ManaType::Blue, 3, false, false),
            hybrid_reserve_unit(ManaType::Blue, 4, false, false),
        ]);
        let mut pool = original.clone();
        let result = pay_cost_with_demand_and_choices(
            &mut pool,
            &parse_mtgjson_mana_cost("{Z}{C/U}{C}{S}"),
            None,
            None,
            Some(permission),
            None,
            LifePaymentColors::EMPTY,
            &[],
        );
        if permission == ManaSpendPermission::AnyTypeOrColor {
            let (spent, life) = result.unwrap();
            assert_eq!(spent[0].pip_id, ManaPipId(2));
            assert!(life.is_empty());
            assert!(pool.is_empty());
        } else {
            assert!(result.is_err());
            assert_eq!(pool, original);
        }
    }
    for ordinary_alternative in [false, true] {
        let mut units = vec![
            hybrid_reserve_unit(ManaType::Green, 1, true, true),
            hybrid_reserve_unit(ManaType::Colorless, 2, false, true),
            hybrid_reserve_unit(ManaType::Blue, 3, true, false),
            hybrid_reserve_unit(ManaType::Red, 4, false, false),
        ];
        if ordinary_alternative {
            units.push(hybrid_reserve_unit(ManaType::Blue, 5, false, false));
        }
        let original = ManaPool::from_units(units);
        let mut pool = original.clone();
        let result = pay_cost_with_demand_and_choices(
            &mut pool,
            &parse_mtgjson_mana_cost("{Z}{C/U}{S}{S}"),
            None,
            None,
            None,
            None,
            LifePaymentColors::EMPTY,
            &[],
        );
        if ordinary_alternative {
            let (spent, life) = result.unwrap();
            assert_eq!(
                spent.iter().map(|u| u.pip_id.0).collect::<Vec<_>>(),
                [2, 5, 1, 3]
            );
            assert!(life.is_empty());
            assert_eq!(pool.units().next().unwrap().pip_id, ManaPipId(4));
        } else {
            assert!(result.is_err());
            assert_eq!(pool, original);
        }
    }
}

#[test]
fn snow_pool_z_partial_payment_leaves_snow_for_auto_tap() {
    // Supplemental Z/S composition: no printed card combines Experiment
    // Five's Z with Wowzer's S. Highland Forest supplies a real Snow/Z unit.
    // CR 601.2g + CR 107.4h: reserve that floating unit for Z and auto-tap
    // Snow-Covered Forest for S. An ordinary Forest cannot supply the deficit.
    // Supplemental strict-U variants combine those axes with colored costs
    // (CR 107.4a): Tropical Island's blue must remain available for each U.
    // CR 107.4c: Spatial Contortion's strict C axis also needs its own mana;
    // Adarkar Wastes supplies C while its W/U ability makes that source Z-eligible.
    // Ulalek, Fused Atrocity supplies all five C/color hybrid axes (CR 107.4e).
    // No printed card is claimed to combine these Z/strict/S requirements.
    for (cost, blue_sources, colorless_sources) in [
        ("{Z}{S}", 0, 0),
        ("{Z}{U}{S}", 1, 0),
        ("{Z}{U}{U}{S}", 2, 0),
        ("{S}{U}{Z}", 1, 0),
        ("{Z}{C}{S}", 0, 1),
        ("{Z}{C}{C}{S}", 0, 2),
        ("{Z}{C/W}{S}", 0, 1),
        ("{Z}{C/U}{S}", 0, 1),
        ("{Z}{C/B}{S}", 0, 1),
        ("{Z}{C/R}{S}", 0, 1),
        ("{Z}{C/G}{S}", 0, 1),
        ("{Z}{C/U}{C/U}{S}", 0, 2),
    ] {
        for with_snow_forest in [true, false] {
            let mut s = board();
            let highland = s
                .add_land_from_oracle(
                    P0,
                    "Highland Forest",
                    "({T}: Add {R} or {G}.)\nThis land enters tapped.",
                )
                .as_snow()
                .with_subtypes(vec!["Mountain", "Forest"])
                .id();
            let forest = with_snow_forest.then(|| snow_forest(&mut s));
            let ordinary_forest = s.add_basic_land(P0, ManaColor::Green);
            let mut reserved_sources: Vec<_> = (0..blue_sources)
                .map(|_| {
                    (
                        s.add_land_from_oracle(P0, "Tropical Island", "({T}: Add {G} or {U}.)")
                            .with_subtypes(vec!["Forest", "Island"])
                            .id(),
                        ManaType::Blue,
                    )
                })
                .collect();
            reserved_sources.extend((0..colorless_sources).map(|_| {
                (
                    s.add_land_from_oracle(
                        P0,
                        "Adarkar Wastes",
                        "{T}: Add {C}.\n{T}: Add {W} or {U}. This land deals 1 damage to you.",
                    )
                    .id(),
                    ManaType::Colorless,
                )
            }));
            let creature = s
                .add_creature_to_hand_from_oracle(P0, "Supplemental Z/strict/S payment", 1, 1, "")
                .with_mana_cost(parse_mtgjson_mana_cost(cost))
                .id();
            let mut r = s.build();
            if let Some(forest) = forest {
                finish_snow_forest(&mut r, forest);
            }
            // Start on a later turn, after Highland Forest has untapped.
            r.state_mut().objects.get_mut(&highland).unwrap().tapped = false;
            evaluate_layers(r.state_mut());
            let (_, _, grouped) = legal_actions_full(r.state());
            let action = grouped[&highland]
                .iter()
                .find(|action| {
                    matches!(action, GameAction::TapLandForMana { selection }
                    if selection.mana_type == ManaType::Green)
                })
                .expect("Highland Forest exposes its green mana action")
                .clone();
            r.act(action).unwrap();
            assert_complete(&r, 1);
            assert!(r.state().objects[&highland].tapped);
            let floating = r.state().players[0].mana_pool.units().next().unwrap();
            assert_eq!(floating.source_id, highland);
            assert_eq!(floating.supertype, Some(ManaSupertype::Snow));
            assert!(floating.source_could_produce_two_or_more_colors);
            // Append ordinary strict-type mana after the snow green, using actual mana actions.
            // Each source can produce two colors, so either pip could pay Z.
            for (source, mana_type) in &reserved_sources {
                let (_, _, grouped) = legal_actions_full(r.state());
                let action = grouped[source]
                    .iter()
                    .find(|action| {
                        matches!(action, GameAction::TapLandForMana { selection }
                        if selection.mana_type == *mana_type)
                    })
                    .expect("source exposes its strict-type mana action")
                    .clone();
                r.act(action).unwrap();
                assert!(r.state().objects[source].tapped);
            }
            let floated: Vec<_> = r.state().players[0].mana_pool.units().collect();
            assert_eq!(floated.len(), 1 + reserved_sources.len());
            assert_eq!(floated[0].source_id, highland);
            for (unit, (source, mana_type)) in floated[1..].iter().zip(&reserved_sources) {
                assert_eq!(unit.source_id, *source);
                assert_eq!(unit.color, *mana_type);
                assert_eq!(unit.supertype, None);
                assert!(unit.source_could_produce_two_or_more_colors);
            }

            let card_id = r.state().objects[&creature].card_id;
            let result = r.act(GameAction::CastSpell {
                object_id: creature,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            });
            assert!(
                !r.state().objects[&ordinary_forest].tapped,
                "ordinary green cannot fill the Snow or Z deficit"
            );
            if let Some(forest) = forest {
                result.expect("floating Z plus auto-tapped Snow pays the full cost");
                assert!(r.state().objects[&forest].tapped);
                assert_eq!(r.state().stack.len(), 1);
                assert!(r.state().players[0].mana_pool.is_empty());
                r.advance_until_stack_empty();
                assert_eq!(r.state().objects[&creature].zone, Zone::Battlefield);
                assert_complete(&r, 0);
            } else {
                assert!(
                    result.is_err(),
                    "ordinary Forest cannot replace the Snow source"
                );
                assert_eq!(r.state().objects[&creature].zone, Zone::Hand);
                assert_eq!(
                    r.state().players[0]
                        .mana_pool
                        .units()
                        .next()
                        .unwrap()
                        .pip_id,
                    floated[0].pip_id,
                    "failed payment preserves the pre-floated unit"
                );
                assert_eq!(
                    r.state().players[0].mana_pool.units().collect::<Vec<_>>(),
                    floated
                );
                assert_complete(&r, 1 + reserved_sources.len());
            }
        }
    }
}

#[test]
fn mycosynth_lattice_preserves_snow_for_printed_wowzer() {
    // CR 609.4b + CR 107.4h: Lattice lets ordinary R pay U, but cannot turn
    // that R into snow mana. Keep the snow Island's U for Wowzer's printed S.
    for krrik in [false, true] {
        let mut s = board();
        s.add_artifact_from_oracle(P0, "Mycosynth Lattice",
            "All permanents are artifacts in addition to their other types.\nAll cards that aren't on the battlefield, spells, and permanents are colorless.\nPlayers may spend mana as though it were mana of any color.");
        let island = s
            .add_land_from_oracle(P0, "Snow-Covered Island", "({T}: Add {U}.)")
            .as_snow()
            .with_subtypes(vec!["Island"])
            .id();
        let wastes = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
        let plains = s.add_basic_land(P0, ManaColor::White);
        let swamp = s.add_basic_land(P0, ManaColor::Black);
        let mountain = s.add_basic_land(P0, ManaColor::Red);
        let forest = s.add_basic_land(P0, ManaColor::Green);
        let second_mountain = s.add_basic_land(P0, ManaColor::Red);
        if krrik {
            s.add_creature_from_oracle(P0, "K'rrik, Son of Yawgmoth", 2, 2,
                "({B/P} can be paid with either {B} or 2 life.)\nLifelink\nFor each {B} in a cost, you may pay 2 life rather than pay that mana.\nWhenever you cast a black spell, put a +1/+1 counter on K'rrik.")
                .as_legendary().with_subtypes(vec!["Phyrexian", "Horror", "Minion"]);
        }
        let wurm = s.add_creature_to_hand_from_oracle(P0, "Wowzer, the Aspirational", 10, 10,
            "Whenever Wowzer, the Aspirational attacks, if you have an {E}, control a Blood, a Clue, a Food, a Map, a Powerstone, and a Treasure, are the monarch, and have the city's blessing and the initiative, you win the game.")
            .as_legendary().as_snow().with_subtypes(vec!["Wurm"])
            .with_mana_cost(parse_mtgjson_mana_cost("{C}{W}{U}{B}{R}{G}{S}")).id();
        let mut r = s.build();
        finish_snow_forest(&mut r, island);
        evaluate_layers(r.state_mut());
        // Place a spare R before B/G so a permitted substitution cannot be
        // confused with the separate future-colored-demand invariant.
        let sources = [
            (island, ManaType::Blue),
            (wastes, ManaType::Colorless),
            (plains, ManaType::White),
            (second_mountain, ManaType::Red),
            (swamp, ManaType::Black),
            (mountain, ManaType::Red),
            (forest, ManaType::Green),
        ];
        for (source, color) in sources {
            let (_, _, grouped) = legal_actions_full(r.state());
            let action = grouped[&source]
                .iter()
                .find(|action| {
                    matches!(action,
                GameAction::TapLandForMana { selection } if selection.mana_type == color)
                })
                .expect("real land exposes its mana action")
                .clone();
            r.act(action).unwrap();
        }
        assert_complete(&r, 7);
        assert_eq!(
            r.state().players[0]
                .mana_pool
                .units()
                .filter(|u| u.supertype == Some(engine::types::mana::ManaSupertype::Snow))
                .count(),
            1
        );
        if krrik {
            r.act(GameAction::CastSpell {
                object_id: wurm,
                card_id: r.state().objects[&wurm].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
            let WaitingFor::PhyrexianPayment { shards, .. } = &r.state().waiting_for else {
                panic!("K'rrik must expose the deferred black choice");
            };
            assert_eq!(shards.len(), 1);
            assert_eq!(shards[0].shard_index, 3);
            assert_eq!(shards[0].options, ShardOptions::ManaOrLife);
            r.act(GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayMana],
            })
            .unwrap();
            r.advance_until_stack_empty();
            assert_eq!(r.state().objects[&wurm].zone, Zone::Battlefield);
        } else {
            r.cast(wurm)
                .resolve()
                .assert_zone(&[wurm], Zone::Battlefield);
        }
        assert!(sources.iter().all(|(id, _)| r.state().objects[id].tapped));
        assert_complete(&r, 0);
    }
}

#[test]
fn snow_pool_payment_precedes_two_generic_hybrid_fallback() {
    // Supplemental combined shape: no printing combines Snow and two-brid.
    // Wowzer supplies the printed Snow requirement; Reaper King's printed
    // {2/W}{2/U}{2/B}{2/R}{2/G} supplies two-brid. CR 107.4e + CR 107.4h:
    // reserve Snow before spending two ordinary units for the hybrid symbol.
    for (text, phyrexian_index) in [
        ("{S}{2/U}", None),
        ("{2/U}{S}", None),
        ("{S}{2/U}{R/P}", Some(2)),
    ] {
        let mut units = vec![
            ManaUnit {
                supertype: Some(ManaSupertype::Snow),
                ..snow_regression_unit(ManaType::Green, 1)
            },
            snow_regression_unit(ManaType::Red, 2),
            snow_regression_unit(ManaType::Red, 3),
        ];
        if phyrexian_index.is_some() {
            units.push(snow_regression_unit(ManaType::Red, 4));
        }
        let mut pool = ManaPool::from_units(units);
        let cost = parse_mtgjson_mana_cost(text);
        assert!(
            can_pay_for_spell(&pool, &cost, None, CostPermissionContext::default()),
            "{text}"
        );
        let choices = [ShardChoice::PayMana];
        if let Some(index) = phyrexian_index {
            let preview = compute_phyrexian_shards(
                &pool,
                &cost,
                None,
                CostPermissionContext {
                    max_life: 1,
                    ..Default::default()
                },
            );
            assert_eq!(preview.len(), 1);
            assert_eq!(preview[0].shard_index, index);
            assert_eq!(preview[0].options, ShardOptions::ManaOrLife);
        }
        let (spent, life) = pay_cost_with_demand_and_choices(
            &mut pool,
            &cost,
            None,
            None,
            None,
            phyrexian_index.map(|_| choices.as_slice()),
            LifePaymentColors::EMPTY,
            &[],
        )
        .unwrap();
        assert_eq!(
            spent[0].pip_id.0, 1,
            "Snow is reserved before the generic fallback"
        );
        assert!(life.is_empty());
        assert!(pool.is_empty());
    }
}

fn snow_regression_unit(color: ManaType, id: u64) -> engine::types::mana::ManaUnit {
    engine::types::mana::ManaUnit {
        color,
        source_id: ObjectId(id),
        pip_id: engine::types::mana::ManaPipId(id),
        supertype: None,
        source_could_produce_two_or_more_colors: false,
        restrictions: vec![],
        grants: vec![],
        expiry: None,
    }
}

#[test]
fn snow_pool_payment_preserves_future_phyrexian_mana_and_explicit_life() {
    // Supplemental U/S/U-P shape, not a printed-card claim. The real printed
    // Wowzer + K'rrik case below covers the same future-mana reservation.
    // CR 107.4f + CR 107.4h: S can use G, leaving U/P payable with U or life.
    let pool = ManaPool::from_units(vec![
        ManaUnit {
            supertype: Some(ManaSupertype::Snow),
            ..snow_regression_unit(ManaType::Blue, 1)
        },
        snow_regression_unit(ManaType::Blue, 2),
        ManaUnit {
            supertype: Some(ManaSupertype::Snow),
            ..snow_regression_unit(ManaType::Green, 3)
        },
    ]);
    let cost = parse_mtgjson_mana_cost("{U}{S}{U/P}");
    let preview = compute_phyrexian_shards(
        &pool,
        &cost,
        None,
        CostPermissionContext {
            max_life: 1,
            ..Default::default()
        },
    );
    assert_eq!(preview.len(), 1);
    assert_eq!(preview[0].shard_index, 2);
    assert_eq!(preview[0].options, ShardOptions::ManaOrLife);
    for choice in [None, Some(ShardChoice::PayMana), Some(ShardChoice::PayLife)] {
        let mut payment_pool = pool.clone();
        let choices = choice.map(|choice| [choice]);
        let (spent, life) = pay_cost_with_demand_and_choices(
            &mut payment_pool,
            &cost,
            None,
            None,
            None,
            choices.as_ref().map(|choices| choices.as_slice()),
            LifePaymentColors::EMPTY,
            &[],
        )
        .unwrap();
        if choice == Some(ShardChoice::PayLife) {
            assert_eq!(life.len(), 1);
            assert_eq!(life[0].amount, 2);
            assert_eq!(
                spent.iter().map(|unit| unit.pip_id.0).collect::<Vec<_>>(),
                vec![2, 1]
            );
            assert_eq!(
                payment_pool
                    .units()
                    .map(|unit| unit.pip_id.0)
                    .collect::<Vec<_>>(),
                vec![3]
            );
        } else {
            assert!(life.is_empty());
            assert_eq!(payment_pool.total(), 0);
            assert_eq!(
                spent.iter().map(|unit| unit.pip_id.0).collect::<Vec<_>>(),
                vec![2, 3, 1]
            );
        }
    }
}

#[test]
fn wowzer_snow_payment_is_invariant_to_cost_symbol_order() {
    // CR 601.2h + CR 107.4h: payment order is not prescribed by symbol order.
    // Wowzer is printed {C}{W}{U}{B}{R}{G}{S}. The second representation below
    // deliberately permutes that same multiset; it is not another printed cost.
    // Floating snow G before snow U makes an early S steal the only G unless
    // the spender reserves future strict requirements before choosing snow.
    for (cost, black_index) in [("{C}{W}{U}{B}{R}{G}{S}", 3), ("{C}{W}{U}{S}{B}{R}{G}", 4)] {
        for (life_permission, reserve_black) in [(false, false), (true, false), (true, true)] {
            let mut s = board();
            let forest = snow_forest(&mut s);
            let (snow_name, snow_text, snow_subtype, snow_color, ordinary_color, other_color) =
                if reserve_black {
                    (
                        "Snow-Covered Swamp",
                        "({T}: Add {B}.)",
                        "Swamp",
                        ManaType::Black,
                        ManaColor::Green,
                        ManaColor::Blue,
                    )
                } else {
                    (
                        "Snow-Covered Island",
                        "({T}: Add {U}.)",
                        "Island",
                        ManaType::Blue,
                        ManaColor::Blue,
                        ManaColor::Black,
                    )
                };
            let snow_land = s
                .add_land_from_oracle(P0, snow_name, snow_text)
                .as_snow()
                .with_subtypes(vec![snow_subtype])
                .id();
            let ordinary_land = s.add_basic_land(P0, ordinary_color);
            let wastes = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
            let plains = s.add_basic_land(P0, ManaColor::White);
            let other_basic = s.add_basic_land(P0, other_color);
            let mountain = s.add_basic_land(P0, ManaColor::Red);
            if life_permission {
                s.add_creature_from_oracle(P0, "K'rrik, Son of Yawgmoth", 2, 2,
                    "({B/P} can be paid with either {B} or 2 life.)\nLifelink\nFor each {B} in a cost, you may pay 2 life rather than pay that mana.\nWhenever you cast a black spell, put a +1/+1 counter on K'rrik.")
                    .as_legendary().with_subtypes(vec!["Phyrexian", "Horror", "Minion"]);
            }
            let wurm = s.add_creature_to_hand_from_oracle(P0, "Wowzer, the Aspirational", 10, 10,
                "Whenever Wowzer, the Aspirational attacks, if you have an {E}, control a Blood, a Clue, a Food, a Map, a Powerstone, and a Treasure, are the monarch, and have the city's blessing and the initiative, you win the game.")
                .as_legendary().as_snow().with_subtypes(vec!["Wurm"])
                .with_mana_cost(parse_mtgjson_mana_cost(cost)).id();
            let mut r = s.build();
            finish_snow_forest(&mut r, forest);
            finish_snow_forest(&mut r, snow_land);
            // Settle the assembled fixture's intrinsic land abilities before
            // requesting selections from the public action projection.
            evaluate_layers(r.state_mut());
            let mut sources = [
                (forest, ManaType::Green),
                (snow_land, snow_color),
                (ordinary_land, ManaType::from(ordinary_color)),
                (wastes, ManaType::Colorless),
                (plains, ManaType::White),
                (other_basic, ManaType::from(other_color)),
                (mountain, ManaType::Red),
            ];
            if reserve_black {
                // Printed Wowzer with K'rrik: G uses ordinary G, S must skip
                // the first snow B and use snow G so B can still pay with mana.
                sources.swap(0, 1);
            }
            for (source, color) in sources {
                let (_, _, grouped) = legal_actions_full(r.state());
                let action = grouped[&source]
                    .iter()
                    .find(|action| {
                        matches!(action,
                        GameAction::TapLandForMana { selection }
                            if selection.mana_type == color)
                    })
                    .expect("each real land exposes the required mana action")
                    .clone();
                r.act(action).unwrap();
            }
            assert_complete(&r, 7);
            assert_eq!(
                r.state().players[0]
                    .mana_pool
                    .units()
                    .map(|unit| unit.source_id)
                    .collect::<Vec<_>>(),
                sources.map(|(source, _)| source),
                "public activations preserve the hostile pool insertion order"
            );
            if life_permission {
                r.act(GameAction::CastSpell {
                    object_id: wurm,
                    card_id: r.state().objects[&wurm].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("announce the equivalent cost with K'rrik's live permission");
                let WaitingFor::PhyrexianPayment { shards, .. } = &r.state().waiting_for else {
                    panic!("K'rrik must offer the deferred black payment");
                };
                assert_eq!(shards.len(), 1);
                assert_eq!(shards[0].shard_index, black_index);
                assert_eq!(shards[0].options, ShardOptions::ManaOrLife);
                r.act(GameAction::SubmitPhyrexianChoices {
                    choices: vec![ShardChoice::PayMana],
                })
                .expect("the preview's black-mana option completes payment");
                r.advance_until_stack_empty();
                assert_eq!(r.state().objects[&wurm].zone, Zone::Battlefield);
            } else {
                r.cast(wurm)
                    .resolve()
                    .assert_zone(&[wurm], Zone::Battlefield);
            }
            assert!(sources
                .iter()
                .all(|(source, _)| r.state().objects[source].tapped));
            assert_complete(&r, 0);
        }
    }
}

#[test]
fn snow_aura_output_uses_the_aura_provenance() {
    // CR 106.3 + CR 107.4h: Fertile Ground's unit is not snow just because
    // its enchanted land is snow. Glittering Frost's own Snow supertype does
    // make its additional unit eligible for Frost Raptor's second S.
    for snow_aura in [false, true] {
        let mut s = board();
        let forest = snow_forest(&mut s);
        let aura = if snow_aura {
            s.add_enchantment_from_oracle(P0, "Glittering Frost",
                "Enchant land\nEnchanted land is snow.\nWhenever enchanted land is tapped for mana, its controller adds an additional one mana of any color.")
                .as_snow().with_subtypes(vec!["Aura"]).id()
        } else {
            s.add_enchantment_from_oracle(P0, "Fertile Ground",
                "Enchant land\nWhenever enchanted land is tapped for mana, its controller adds an additional one mana of any color.")
                .with_subtypes(vec!["Aura"]).id()
        };
        let raptor = s.add_creature_from_oracle(P0, "Frost Raptor", 2, 2,
            "Flying\n{S}{S}: This creature gains shroud until end of turn. ({S} can be paid with one mana from a snow source. A creature with shroud can't be the target of spells or abilities.)")
            .as_snow().with_subtypes(vec!["Bird"]).id();
        let mut r = s.build();
        finish_snow_forest(&mut r, forest);
        engine::game::effects::attach::attach_to(r.state_mut(), aura, forest);
        evaluate_layers(r.state_mut());
        let result = r.act(GameAction::ActivateAbility {
            source_id: raptor,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        });
        if snow_aura {
            result.expect("two independently snow sources produce two snow units");
            assert_eq!(r.state().stack.len(), 1);
            r.pass_both_players();
            assert!(r.state().objects[&forest].tapped);
        } else {
            assert!(
                result.is_err(),
                "the ordinary Aura cannot supply the second S"
            );
            assert!(!r.state().objects[&forest].tapped);
        }
        assert_complete(&r, 0);
    }
}

#[test]
fn food_chain_retains_explicit_fodder_choice_during_manual_payment() {
    // CR 601.2g + CR 605.3b: Llanowar Elves may add G before Food Chain exiles
    // it for two more. Food Chain's non-self exile is a player choice, not an
    // automatic source activation: even Auto activation must ask for the Elf.
    for elf_count in [1, 2] {
        let mut s = board();
        let chain = s.add_enchantment_from_oracle(P0, "Food Chain",
            "Exile a creature you control: Add X mana of any one color, where X is 1 plus the exiled creature's mana value. Spend this mana only to cast creature spells.").id();
        let elves: Vec<_> = (0..elf_count)
            .map(|_| {
                s.add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, "{T}: Add {G}.")
                    .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
                    .id()
            })
            .collect();
        let selected_elf = *elves.last().unwrap();
        let forests = [
            s.add_basic_land(P0, ManaColor::Green),
            s.add_basic_land(P0, ManaColor::Green),
        ];
        let dinosaur = s
            .add_creature_to_hand_from_oracle(P0, "Gigantosaurus", 10, 10, "")
            .with_mana_cost(parse_mtgjson_mana_cost("{G}{G}{G}{G}{G}"))
            .id();
        let mut r = s.build();
        let chain_index = assert_any_one(&r, chain);
        let chain_options: Vec<_> = activatable_mana_source_selections(r.state(), P0)
            .into_iter()
            .filter(|selection| selection.source.object_id == chain)
            .collect();
        assert_eq!(
            chain_options.len(),
            1,
            "interactive capability keeps color deferred"
        );
        assert!(matches!(
            chain_options[0].output,
            ManaSourceOutput::DeferredColorChoice {
                quantity: ManaSourceQuantity::Variable
            }
        ));
        assert!(chain_options[0].atomic_combination.is_none());

        r.act(GameAction::CastSpell {
            object_id: dinosaur,
            card_id: r.state().objects[&dinosaur].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .unwrap();
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ));
        assert_eq!(r.state().players[0].mana_pool.total(), 0);
        assert!(elves
            .iter()
            .all(|id| r.state().objects[id].zone == Zone::Battlefield
                && !r.state().objects[id].tapped));
        r.act(GameAction::ActivateAbility {
            source_id: selected_elf,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ));
        assert!(r.state().objects[&selected_elf].tapped);
        assert_eq!(
            r.state().players[0].mana_pool.count_color(ManaType::Green),
            1
        );

        r.act(GameAction::ActivateAbility {
            source_id: chain,
            ability_index: chain_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
        match &r.state().waiting_for {
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExileFromManaZone { zone },
                choices,
                count,
                ..
            } => {
                assert_eq!(*player, P0);
                assert_eq!(*zone, Zone::Battlefield);
                assert_eq!(*count, 1);
                assert_eq!(choices.len(), elves.len());
                assert!(elves.iter().all(|elf| choices.contains(elf)));
            }
            other => panic!("Food Chain must ask which creature to exile, got {other:?}"),
        }
        assert!(elves
            .iter()
            .all(|id| r.state().objects[id].zone == Zone::Battlefield));
        assert_eq!(r.state().players[0].mana_pool.total(), 1);
        r.act(GameAction::SelectCards {
            cards: vec![selected_elf],
        })
        .unwrap();
        choose_color(&mut r, ManaType::Green);
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ));
        assert_eq!(r.state().objects[&selected_elf].zone, Zone::Exile);
        let units: Vec<_> = r.state().players[0].mana_pool.units().collect();
        assert_eq!(units.len(), 3);
        assert!(units.iter().all(|unit| unit.color == ManaType::Green));
        assert_eq!(
            units
                .iter()
                .filter(|unit| unit.source_id == chain && !unit.restrictions.is_empty())
                .count(),
            2
        );
        assert!(forests.iter().all(|id| !r.state().objects[id].tapped));

        r.act(GameAction::PassPriority).unwrap();
        assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(r.state().stack.len(), 1);
        assert!(forests.iter().all(|id| r.state().objects[id].tapped));
        r.pass_both_players();
        assert_eq!(r.state().objects[&dinosaur].zone, Zone::Battlefield);
        for elf in elves.into_iter().filter(|elf| *elf != selected_elf) {
            assert_eq!(r.state().objects[&elf].zone, Zone::Battlefield);
            assert!(!r.state().objects[&elf].tapped);
        }
        assert_complete(&r, 0);
    }
}

fn choose_color(r: &mut GameRunner, color: ManaType) {
    match &r.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(options.len(), 5);
        }
        other => panic!("expected mandatory single-color prompt, got {other:?}"),
    }
    r.act(GameAction::ChooseManaColor {
        choice: ManaChoice::SingleColor(color),
        count: 1,
    })
    .unwrap();
}

#[test]
fn loot_bundle_avoids_optional_costs_without_starving_required_mana() {
    // CR 702.177a: the mana ability is usable only once. Auto must not spend
    // that activation and its green sub-cost merely for greater bundle coverage.
    for (island_count, birds_funds_green) in
        [(3, false), (2, false), (1, false), (0, false), (0, true)]
    {
        let spare_islands = island_count == 3;
        let mut s = board();
        let loot = s.add_creature(P0, "Loot, the Pathfinder", 2, 4)
            .from_oracle_text_with_keywords(&["Double strike", "Vigilance", "Haste"],
                "Double strike, vigilance, haste\nExhaust — {G}, {T}: Add three mana of any one color. (Activate each exhaust ability only once.)\nExhaust — {U}, {T}: Draw three cards.\nExhaust — {R}, {T}: Loot deals 3 damage to any target.")
            .id();
        let green_source = if birds_funds_green {
            s.add_creature(P0, "Birds of Paradise", 0, 1)
                .from_oracle_text_with_keywords(
                    &["Flying"],
                    "Flying\n{T}: Add one mana of any color.",
                )
                .id()
        } else {
            s.add_basic_land(P0, ManaColor::Green)
        };
        let islands: Vec<_> = (0..island_count)
            .map(|_| s.add_basic_land(P0, ManaColor::Blue))
            .collect();
        let charm = spell(&mut s, "Archmage's Charm", true, CHARM, "{U}{U}{U}");
        let mut r = s.build();
        let ability = assert_any_one(&r, loot);
        assert!(r.state().objects[&loot].abilities[ability]
            .activation_restrictions
            .contains(&ActivationRestriction::OnlyOnce));
        r.cast(charm)
            .modes(&[1])
            .target_player(P0)
            .resolve()
            .assert_hand_drawn(P0, 2);
        assert_eq!(r.state().objects[&loot].tapped, !spare_islands);
        assert_eq!(r.state().objects[&green_source].tapped, !spare_islands);
        assert!(islands
            .iter()
            .all(|id| r.state().objects[id].tapped == spare_islands));
        // This witness covers auto-payment source selection. The automatic
        // resolver's separate once-only activation ledger is not asserted here.
        assert_complete(&r, 0);
    }
}

#[test]
fn equal_output_prefers_city_of_brass_to_birds_as_a_blocker() {
    let mut s = board();
    // Add the creature first so object order cannot stand in for card tiers.
    let birds = s
        .add_creature(P0, "Birds of Paradise", 0, 1)
        .from_oracle_text_with_keywords(&["Flying"], "Flying\n{T}: Add one mana of any color.")
        .id();
    let city = s.add_land_from_oracle(P0, "City of Brass",
        "Whenever this land becomes tapped, it deals 1 damage to you.\n{T}: Add one mana of any color.").id();
    let reach = spell(&mut s, "Reach Through Mists", true, "Draw a card.", "{U}");
    let mut r = s.build();
    let outcome = r.cast(reach).resolve();
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_life_delta(P0, -1);
    assert!(r.state().objects[&city].tapped);
    assert!(!r.state().objects[&birds].tapped);
    assert_complete(&r, 0);
}

#[test]
fn equal_output_same_card_tier_keeps_within_tier_penalty_order() {
    let mut s = board();
    // The established policy prefers damage-on-resolution to pay-life-on-activation.
    let confluence = s
        .add_land_from_oracle(
            P0,
            "Mana Confluence",
            "{T}, Pay 1 life: Add one mana of any color.",
        )
        .id();
    let city = s.add_land_from_oracle(P0, "City of Brass",
        "Whenever this land becomes tapped, it deals 1 damage to you.\n{T}: Add one mana of any color.").id();
    let reach = spell(&mut s, "Reach Through Mists", true, "Draw a card.", "{U}");
    let mut r = s.build();
    let outcome = r.cast(reach).resolve();
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_life_delta(P0, -1);
    assert!(r.state().objects[&city].tapped);
    assert!(!r.state().objects[&confluence].tapped);
    assert_complete(&r, 0);
}

#[test]
fn two_blue_pips_preserve_goldspan_treasure_when_islands_can_pay() {
    let mut s = board();
    s.add_creature(P0, "Goldspan Dragon", 4, 4)
        .from_oracle_text_with_keywords(&["Flying", "Haste"], GOLDSPAN);
    let treasure = s
        .add_artifact_from_oracle(
            P0,
            "Treasure",
            "{T}, Sacrifice this token: Add one mana of any color.",
        )
        .with_subtypes(vec!["Treasure"])
        .id();
    let islands = [
        s.add_basic_land(P0, ManaColor::Blue),
        s.add_basic_land(P0, ManaColor::Blue),
    ];
    let ideas = spell(
        &mut s,
        "Ideas Unbound",
        false,
        "Draw three cards. Discard three cards at the beginning of the next end step.",
        "{U}{U}",
    );
    let mut r = s.build();
    r.state_mut().objects.get_mut(&treasure).unwrap().is_token = true;
    evaluate_layers(r.state_mut());
    assert_any_one(&r, treasure);
    r.cast(ideas).resolve().assert_hand_drawn(P0, 3);
    assert!(islands.iter().all(|id| r.state().objects[id].tapped));
    assert_eq!(r.state().objects[&treasure].zone, Zone::Battlefield);
    assert!(!r.state().objects[&treasure].tapped);
    assert_complete(&r, 0);
}

#[test]
fn bundle_leftovers_preserve_the_scarcer_hybrid_for_jinnie_fay() {
    let mut s = board();
    let astral = s
        .add_artifact_from_oracle(P0, "Astral Cornucopia", ASTRAL)
        .id();
    s.with_counter(astral, CounterType::Generic("charge".into()), 2);
    // Two red units keep this source-planning regression independent of the
    // pool spender's separate first-hybrid allocation limitation with {G}{G}{R}.
    let alena = s.add_creature(P0, "Alena, Kessig Trapper", 4, 3)
        .from_oracle_text_with_keywords(&["First strike"], "First strike\n{T}: Add an amount of {R} equal to the greatest power among creatures you control that entered this turn.\nPartner (You can have two commanders if both have partner.)")
        .id();
    s.add_creature(P0, "Grizzly Bears", 2, 2)
        .with_summoning_sickness();
    let jinnie = s.add_creature_to_hand_from_oracle(
        P0,
        "Jinnie Fay, Jetmir's Second",
        3,
        3,
        "If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.",
    ).with_mana_cost(parse_mtgjson_mana_cost("{R/G}{G}{G/W}")).id();
    let mut r = s.build();
    assert_any_one(&r, astral);
    r.cast(jinnie)
        .resolve()
        .assert_zone(&[jinnie], Zone::Battlefield);
    assert!(r.state().objects[&astral].tapped);
    assert!(r.state().objects[&alena].tapped);
    assert_eq!(r.state().players[0].mana_pool.count_color(ManaType::Red), 1);
    assert_complete(&r, 1);
}

#[test]
fn incubation_druid_counts_all_three_of_its_selected_type() {
    let mut s = board();
    let druid = s.add_creature(P0, "Incubation Druid", 0, 2)
        .from_oracle_text("{T}: Add one mana of any type that a land you control could produce. If this creature has a +1/+1 counter on it, add three mana of that type instead.\n{3}{G}{G}: Adapt 3. (If this creature has no +1/+1 counters on it, put three +1/+1 counters on it.)").id();
    s.with_counter(druid, CounterType::Plus1Plus1, 1);
    let islands = [
        s.add_basic_land(P0, ManaColor::Blue),
        s.add_basic_land(P0, ManaColor::Blue),
    ];
    let charm = spell(&mut s, "Archmage's Charm", true, CHARM, "{U}{U}{U}");
    let mut r = s.build();
    assert!(r.state().objects[&druid]
        .abilities
        .iter()
        .any(|ability| matches!(
            &*ability.effect,
            Effect::Mana {
                produced: ManaProduction::AnyTypeProduceableBy { .. },
                ..
            }
        )));
    r.cast(charm)
        .modes(&[1])
        .target_player(P0)
        .resolve()
        .assert_hand_drawn(P0, 2);
    assert!(r.state().objects[&druid].tapped);
    assert!(islands.iter().all(|id| !r.state().objects[id].tapped));
    assert_complete(&r, 0);
}

#[test]
fn counted_color_sets_preserve_one_unit_and_empty_choice_controls() {
    for (name, oracle) in [
        ("Mox Amber", "{T}: Add one mana of any color among legendary creatures and planeswalkers you control."),
        ("Exotic Orchard", "{T}: Add one mana of any color that a land an opponent controls could produce."),
        ("Arcane Signet", "{T}: Add one mana of any color in your commander's color identity."),
    ] {
        for available in [false, true] {
            let mut s = board();
            let source = if name == "Exotic Orchard" {
                s.add_land_from_oracle(P0, name, oracle).id()
            } else {
                s.add_artifact_from_oracle(P0, name, oracle).id()
            };
            if available {
                if name == "Exotic Orchard" {
                    s.add_basic_land(P1, ManaColor::White);
                } else {
                    // Vanilla legendary Isamaru supplies white, either on the
                    // battlefield for Amber or as Signet's commander identity.
                    let isamaru = s.add_creature(P0, "Isamaru, Hound of Konda", 2, 2)
                        .as_legendary().with_mana_cost(parse_mtgjson_mana_cost("{W}")).id();
                    if name == "Arcane Signet" { s.with_commander(isamaru); }
                }
            }
            let lions = s.add_creature_to_hand_from_oracle(P0, "Savannah Lions", 2, 1, "")
                .with_mana_cost(parse_mtgjson_mana_cost("{W}")).id();
            let mut r = s.build();
            let produced = match &*r.state().objects[&source].abilities[0].effect {
                Effect::Mana { produced, .. } => produced,
                other => panic!("expected parsed mana ability for {name}: {other:?}"),
            };
            assert!(match name {
                "Mox Amber" => matches!(produced, ManaProduction::AnyOneColorAmongPermanents { .. }),
                "Exotic Orchard" => matches!(produced, ManaProduction::OpponentLandColors { .. }),
                "Arcane Signet" => matches!(produced, ManaProduction::AnyInCommandersColorIdentity { .. }),
                _ => unreachable!(),
            });
            assert_eq!(activatable_mana_source_selections(r.state(), P0).iter()
                .any(|selection| selection.source.object_id == source), available, "{name}");
            if available {
                r.cast(lions).resolve().assert_zone(&[lions], Zone::Battlefield);
                assert!(r.state().objects[&source].tapped);
            } else {
                assert!(!r.state().objects[&source].tapped);
            }
            assert_complete(&r, 0);
        }
    }
}

#[test]
fn generic_source_with_mana_cost_revokes_earlier_surplus_credit() {
    let mut s = board();
    let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
    let signet = s
        .add_artifact_from_oracle(P0, "Gruul Signet", "{1}, {T}: Add {R}{G}.")
        .id();
    let wastes = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
    let black_lotus = s
        .add_artifact_from_oracle(
            P0,
            "Black Lotus",
            "{T}, Sacrifice this artifact: Add three mana of any one color.",
        )
        .id();
    let opportunity = spell(
        &mut s,
        "Opportunity",
        true,
        "Target player draws four cards.",
        "{4}{U}{U}",
    );
    let mut r = s.build();
    assert_any_one(&r, lotus);
    assert_any_one(&r, black_lotus);
    r.cast(opportunity)
        .target_player(P0)
        .resolve()
        .assert_hand_drawn(P0, 4);
    assert!([lotus, signet, wastes]
        .iter()
        .all(|id| r.state().objects[id].tapped));
    assert_eq!(r.state().objects[&black_lotus].zone, Zone::Graveyard);
    assert_complete(&r, 2);
}

#[test]
fn free_bundle_surplus_cannot_also_fund_another_sources_activation() {
    let mut s = board();
    let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
    let signet = s
        .add_artifact_from_oracle(P0, "Dimir Signet", "{1}, {T}: Add {U}{B}.")
        .id();
    let wastes = [
        s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id(),
        s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id(),
    ];
    let muldrotha = s.add_creature_to_hand_from_oracle(P0, "Muldrotha, the Gravetide", 6, 6,
        "During each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard. (If a card has multiple permanent types, choose one as you play it.)")
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{B}{G}{U}")).id();
    let mut r = s.build();
    assert_any_one(&r, lotus);
    r.cast(muldrotha)
        .resolve()
        .assert_zone(&[muldrotha], Zone::Battlefield);
    assert!([lotus, signet, wastes[0], wastes[1]]
        .iter()
        .all(|id| r.state().objects[id].tapped));
    assert_complete(&r, 0);
}

#[test]
fn counted_bundle_fallback_preserves_nested_filter_funding() {
    let mut s = board();
    let loot = s.add_creature(P0, "Loot, the Pathfinder", 2, 4)
        .from_oracle_text_with_keywords(&["Double strike", "Vigilance", "Haste"],
            "Double strike, vigilance, haste\nExhaust — {G}, {T}: Add three mana of any one color. (Activate each exhaust ability only once.)\nExhaust — {U}, {T}: Draw three cards.\nExhaust — {R}, {T}: Loot deals 3 damage to any target.")
        .id();
    let signet = s
        .add_artifact_from_oracle(P0, "Gruul Signet", "{1}, {T}: Add {R}{G}.")
        .id();
    let wastes = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
    let charm = spell(&mut s, "Archmage's Charm", true, CHARM, "{U}{U}{U}");
    let mut r = s.build();
    assert_any_one(&r, loot);
    r.cast(charm)
        .modes(&[1])
        .target_player(P0)
        .resolve()
        .assert_hand_drawn(P0, 2);
    assert!([loot, signet, wastes]
        .iter()
        .all(|id| r.state().objects[id].tapped));
    assert_eq!(r.state().players[0].mana_pool.count_color(ManaType::Red), 1);
    assert_complete(&r, 1);
}

#[test]
fn counted_bundle_with_multiple_mana_sub_costs_keeps_scalar_fallback_payable() {
    let mut s = board();
    let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
    let dimir = s
        .add_artifact_from_oracle(P0, "Dimir Signet", "{1}, {T}: Add {U}{B}.")
        .id();
    let gruul = s
        .add_artifact_from_oracle(P0, "Gruul Signet", "{1}, {T}: Add {R}{G}.")
        .id();
    let wastes = [
        s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id(),
        s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id(),
    ];
    let muldrotha = s.add_creature_to_hand_from_oracle(P0, "Muldrotha, the Gravetide", 6, 6,
        "During each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard. (If a card has multiple permanent types, choose one as you play it.)")
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{B}{G}{U}")).id();
    let mut r = s.build();
    assert_any_one(&r, lotus);
    r.cast(muldrotha)
        .resolve()
        .assert_zone(&[muldrotha], Zone::Battlefield);
    assert!([lotus, dimir, gruul, wastes[0], wastes[1]]
        .iter()
        .all(|id| r.state().objects[id].tapped));
    assert_complete(&r, 1);
}

#[test]
fn generic_counted_bundle_keeps_its_activation_funding() {
    let mut s = board();
    let loot = s.add_creature(P0, "Loot, the Pathfinder", 2, 4)
        .from_oracle_text_with_keywords(&["Double strike", "Vigilance", "Haste"],
            "Double strike, vigilance, haste\nExhaust — {G}, {T}: Add three mana of any one color. (Activate each exhaust ability only once.)\nExhaust — {U}, {T}: Draw three cards.\nExhaust — {R}, {T}: Loot deals 3 damage to any target.")
        .id();
    let forests = [
        s.add_basic_land(P0, ManaColor::Green),
        s.add_basic_land(P0, ManaColor::Green),
    ];
    let black_lotus = s
        .add_artifact_from_oracle(
            P0,
            "Black Lotus",
            "{T}, Sacrifice this artifact: Add three mana of any one color.",
        )
        .id();
    let eye = s
        .add_artifact_to_hand_from_oracle(
            P0,
            "Mind's Eye",
            "Whenever an opponent draws a card, you may pay {1}. If you do, draw a card.",
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{5}"))
        .id();
    let mut r = s.build();
    assert_any_one(&r, loot);
    assert_any_one(&r, black_lotus);
    r.cast(eye).resolve().assert_zone(&[eye], Zone::Battlefield);
    assert!(r.state().objects[&loot].tapped);
    assert!(forests.iter().all(|id| r.state().objects[id].tapped));
    assert_eq!(r.state().objects[&black_lotus].zone, Zone::Graveyard);
    assert_complete(&r, 2);
}

#[test]
fn colored_counted_bundle_pays_activation_before_floating_generic_funding() {
    // CR 118.10: all three blue units are committed to Teferi, so the green
    // activation cost must not consume either of the two outer generic units.
    for (island_count, float_green) in [(0, false), (1, false), (2, false), (2, true)] {
        let mut s = board();
        let loot = s.add_creature(P0, "Loot, the Pathfinder", 2, 4)
            .from_oracle_text_with_keywords(&["Double strike", "Vigilance", "Haste"],
                "Double strike, vigilance, haste\nExhaust — {G}, {T}: Add three mana of any one color. (Activate each exhaust ability only once.)\nExhaust — {U}, {T}: Draw three cards.\nExhaust — {R}, {T}: Loot deals 3 damage to any target.")
            .id();
        let islands: Vec<_> = (0..island_count)
            .map(|_| s.add_basic_land(P0, ManaColor::Blue))
            .collect();
        let forests: Vec<_> = (0..3 - island_count)
            .map(|_| s.add_basic_land(P0, ManaColor::Green))
            .collect();
        let teferi = s.add_creature_to_hand_from_oracle(P0, "Teferi, Mage of Zhalfir", 3, 4,
            "Flash\nCreature cards you own that aren't on the battlefield have flash.\nEach opponent can cast spells only any time they could cast a sorcery.")
            .as_legendary().with_mana_cost(parse_mtgjson_mana_cost("{2}{U}{U}{U}")).id();
        let mut r = s.build();
        assert_any_one(&r, loot);
        if float_green {
            // An existing pool rejects the empty-pool proof. The scalar
            // fallback must still replay Loot's actual three-unit output.
            let (_, _, grouped) = legal_actions_full(r.state());
            let action = grouped[&forests[0]]
                .iter()
                .find(|action| {
                    matches!(action, GameAction::TapLandForMana { selection }
                    if selection.mana_type == ManaType::Green)
                })
                .expect("Forest exposes its green mana action")
                .clone();
            r.act(action).unwrap();
            assert_complete(&r, 1);
        }
        r.cast(teferi)
            .resolve()
            .assert_zone(&[teferi], Zone::Battlefield);
        assert!(r.state().objects[&loot].tapped);
        assert!(islands
            .iter()
            .chain(&forests)
            .all(|id| r.state().objects[id].tapped));
        assert_complete(&r, 0);
    }
}

#[test]
fn counted_bundle_funds_its_cost_before_outer_generic_mana() {
    for forest_count in [2, 3] {
        let mut s = board();
        let loot = s.add_creature(P0, "Loot, the Pathfinder", 2, 4)
            .from_oracle_text_with_keywords(&["Double strike", "Vigilance", "Haste"],
                "Double strike, vigilance, haste\nExhaust — {G}, {T}: Add three mana of any one color. (Activate each exhaust ability only once.)\nExhaust — {U}, {T}: Draw three cards.\nExhaust — {R}, {T}: Loot deals 3 damage to any target.")
            .id();
        let forests: Vec<_> = (0..forest_count)
            .map(|_| s.add_basic_land(P0, ManaColor::Green))
            .collect();
        let concentrate = spell(
            &mut s,
            "Concentrate",
            false,
            "Draw three cards.",
            "{2}{U}{U}",
        );
        let mut r = s.build();
        assert_any_one(&r, loot);
        r.cast(concentrate).resolve().assert_hand_drawn(P0, 3);
        assert!(r.state().objects[&loot].tapped);
        assert_eq!(
            forests
                .iter()
                .filter(|id| r.state().objects[id].tapped)
                .count(),
            2
        );
        assert_complete(&r, 0);
    }
}

#[test]
fn costed_bundle_surplus_does_not_spend_the_spells_generic_funding() {
    for signet_count in [1, 2] {
        for include_lotus in [false, true] {
            let mut s = board();
            let signets: Vec<_> = (0..signet_count)
                .map(|_| {
                    s.add_artifact_from_oracle(P0, "Dimir Signet", "{1}, {T}: Add {U}{B}.")
                        .id()
                })
                .collect();
            let forests = [
                s.add_basic_land(P0, ManaColor::Green),
                s.add_basic_land(P0, ManaColor::Green),
            ];
            let lotus =
                include_lotus.then(|| s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id());
            let (name, text, cost, drawn) = if signet_count == 1 {
                ("Divination", "Draw two cards.", "{2}{U}", 2)
            } else {
                ("Concentrate", "Draw three cards.", "{2}{U}{U}", 3)
            };
            let draw = spell(&mut s, name, false, text, cost);
            let mut r = s.build();
            r.cast(draw).resolve().assert_hand_drawn(P0, drawn);
            assert!(signets.iter().all(|id| r.state().objects[id].tapped));
            assert!(forests.iter().all(|id| r.state().objects[id].tapped));
            if let Some(lotus) = lotus {
                assert!(!r.state().objects[&lotus].tapped);
            }
            assert_complete(&r, 0);
        }
    }
}

#[test]
fn ordinary_two_island_payment_preserves_lotus_for_a_later_green_spell() {
    let mut s = board();
    let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
    let islands = [
        s.add_basic_land(P0, ManaColor::Blue),
        s.add_basic_land(P0, ManaColor::Blue),
    ];
    let ideas = spell(
        &mut s,
        "Ideas Unbound",
        false,
        "Draw three cards. Discard three cards at the beginning of the next end step.",
        "{U}{U}",
    );
    let elves = s
        .add_creature_to_hand_from_oracle(P0, "Llanowar Elves", 1, 1, "{T}: Add {G}.")
        .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
        .id();
    let mut r = s.build();
    assert_any_one(&r, lotus);
    r.cast(ideas).resolve().assert_hand_drawn(P0, 3);
    assert!(islands.iter().all(|id| r.state().objects[id].tapped));
    assert!(!r.state().objects[&lotus].tapped);
    assert_complete(&r, 0);
    r.cast(elves)
        .resolve()
        .assert_zone(&[elves], Zone::Battlefield);
    assert!(r.state().objects[&lotus].tapped);
    assert_eq!(
        r.state().players[0].mana_pool.count_color(ManaType::Green),
        2
    );
    assert_complete(&r, 2);
}

#[test]
fn one_blue_pip_preserves_lotus_when_island_can_pay() {
    let mut s = board();
    let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
    let island = s.add_basic_land(P0, ManaColor::Blue);
    let mists = spell(&mut s, "Reach Through Mists", true, "Draw a card.", "{U}");
    let mut r = s.build();
    assert_any_one(&r, lotus);
    r.cast(mists).resolve().assert_hand_drawn(P0, 1);
    assert!(r.state().objects[&island].tapped);
    assert!(!r.state().objects[&lotus].tapped);
    assert_complete(&r, 0);
}

#[test]
fn one_blue_pip_preserves_goldspan_treasure_when_island_can_pay() {
    let mut s = board();
    s.add_creature(P0, "Goldspan Dragon", 4, 4)
        .from_oracle_text_with_keywords(&["Flying", "Haste"], GOLDSPAN);
    let treasure = s
        .add_artifact_from_oracle(
            P0,
            "Treasure",
            "{T}, Sacrifice this token: Add one mana of any color.",
        )
        .with_subtypes(vec!["Treasure"])
        .id();
    let island = s.add_basic_land(P0, ManaColor::Blue);
    let mists = spell(&mut s, "Reach Through Mists", true, "Draw a card.", "{U}");
    let mut r = s.build();
    r.state_mut().objects.get_mut(&treasure).unwrap().is_token = true;
    evaluate_layers(r.state_mut());
    assert_any_one(&r, treasure);
    r.cast(mists).resolve().assert_hand_drawn(P0, 1);
    assert!(r.state().objects[&island].tapped);
    assert_eq!(r.state().objects[&treasure].zone, Zone::Battlefield);
    assert!(!r.state().objects[&treasure].tapped);
    assert_complete(&r, 0);
}

#[test]
fn lotus_chooses_growth_spiral_color_missing_from_basic_land() {
    for basic_color in [ManaColor::Blue, ManaColor::Green] {
        let mut s = board();
        let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
        let land = s.add_basic_land(P0, basic_color);
        let spiral = spell(
            &mut s,
            "Growth Spiral",
            true,
            "Draw a card. You may put a land card from your hand onto the battlefield.",
            "{G}{U}",
        );
        let mut r = s.build();
        assert_any_one(&r, lotus);
        r.cast(spiral)
            .resolve()
            .assert_zone(&[spiral], Zone::Graveyard);
        assert!(r.state().objects[&lotus].tapped);
        assert!(r.state().objects[&land].tapped);
        assert_complete(&r, 2);
    }
}

#[test]
fn lotus_pays_all_colored_shards_without_spare_islands() {
    let mut s = board();
    let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
    let islands = [
        s.add_basic_land(P0, ManaColor::Blue),
        s.add_basic_land(P0, ManaColor::Blue),
    ];
    let charm = spell(&mut s, "Archmage's Charm", true, CHARM, "{U}{U}{U}");
    let mut r = s.build();
    assert_any_one(&r, lotus);
    assert_complete(&r, 0);
    assert!(islands.iter().all(|id| !r.state().objects[id].tapped));
    let out = r.cast(charm).modes(&[1]).target_player(P0).resolve();
    out.assert_hand_drawn(P0, 2);
    out.assert_zone(&[charm], Zone::Graveyard);
    assert!(r.state().objects[&lotus].tapped);
    assert!(islands.iter().all(|id| !r.state().objects[id].tapped));
    assert_complete(&r, 0);
}

#[test]
fn lotus_surplus_pays_divination_generic_without_spare_forests() {
    let mut s = board();
    let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
    let forests = [
        s.add_basic_land(P0, ManaColor::Green),
        s.add_basic_land(P0, ManaColor::Green),
    ];
    let divination = spell(&mut s, "Divination", false, "Draw two cards.", "{2}{U}");
    let mut r = s.build();
    assert_complete(&r, 0);
    let out = r.cast(divination).resolve();
    out.assert_hand_drawn(P0, 2);
    out.assert_zone(&[divination], Zone::Graveyard);
    assert!(r.state().objects[&lotus].tapped);
    assert!(forests.iter().all(|id| !r.state().objects[id].tapped));
    assert_complete(&r, 0);
}

#[test]
fn pure_generic_preserves_source_ranking_and_full_lotus_output() {
    for spare_islands in [false, true] {
        let mut s = board();
        let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
        let islands: Vec<_> = (0..if spare_islands { 2 } else { 0 })
            .map(|_| s.add_basic_land(P0, ManaColor::Blue))
            .collect();
        let stone = s
            .add_artifact_to_hand_from_oracle(P0, "Mind Stone", MIND_STONE)
            .with_mana_cost(parse_mtgjson_mana_cost("{2}"))
            .id();
        let mut r = s.build();
        r.cast(stone)
            .resolve()
            .assert_zone(&[stone], Zone::Battlefield);
        assert_eq!(r.state().objects[&lotus].tapped, !spare_islands);
        assert!(islands.iter().all(|id| r.state().objects[id].tapped));
        assert_complete(&r, if spare_islands { 0 } else { 1 });
    }
}

#[test]
fn lotus_surplus_pays_two_brid_fallback_after_colored_assignment() {
    let mut s = board();
    let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
    let wastes: Vec<_> = (0..7)
        .map(|_| s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id())
        .collect();
    let king = s
        .add_creature_to_hand_from_oracle(P0, "Reaper King", 6, 6, KING)
        .as_artifact_creature()
        .with_subtypes(vec!["Scarecrow"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2/W}{2/U}{2/B}{2/R}{2/G}"))
        .id();
    let mut r = s.build();
    assert_complete(&r, 0);
    r.cast(king)
        .resolve()
        .assert_zone(&[king], Zone::Battlefield);
    assert!(r.state().objects[&lotus].tapped);
    assert_eq!(
        wastes
            .iter()
            .filter(|id| r.state().objects[id].tapped)
            .count(),
        6
    );
    assert_complete(&r, 0);
}

#[test]
fn fixed_colorless_bundle_surplus_pays_spatial_contortion() {
    let mut s = board();
    let ring = s
        .add_artifact_from_oracle(P0, "Sol Ring", "{T}: Add {C}{C}.")
        .id();
    let island = s.add_basic_land(P0, ManaColor::Blue);
    let baloth = s
        .add_creature_from_oracle(P0, "Leatherback Baloth", 4, 5, "")
        .with_mana_cost(parse_mtgjson_mana_cost("{G}{G}{G}"))
        .id();
    let spatial = spell(&mut s, "Spatial Contortion", true, SPATIAL, "{1}{C}");
    let mut r = s.build();
    let out = r.cast(spatial).target_object(baloth).resolve();
    out.assert_zone(&[spatial], Zone::Graveyard);
    assert_eq!(
        (
            r.state().objects[&baloth].power,
            r.state().objects[&baloth].toughness
        ),
        (Some(7), Some(2))
    );
    assert!(r.state().objects[&ring].tapped);
    assert!(!r.state().objects[&island].tapped);
    assert_complete(&r, 0);
}

#[test]
fn astral_known_counts_include_zero_one_and_three() {
    for count in [0, 1, 3] {
        let mut s = board();
        let astral = s
            .add_artifact_from_oracle(P0, "Astral Cornucopia", ASTRAL)
            .id();
        let charge = CounterType::Generic("charge".into());
        s.with_counter(astral, charge.clone(), count);
        let islands: Vec<_> = (0..if count == 1 { 0 } else { 2 })
            .map(|_| s.add_basic_land(P0, ManaColor::Blue))
            .collect();
        let card = if count == 3 {
            spell(&mut s, "Archmage's Charm", true, CHARM, "{U}{U}{U}")
        } else {
            spell(&mut s, "Reach Through Mists", true, "Draw a card.", "{U}")
        };
        let mut r = s.build();
        assert_any_one(&r, astral);
        let selections = activatable_mana_source_selections(r.state(), P0);
        assert_eq!(
            selections.iter().any(|x| x.source.object_id == astral),
            count > 0
        );
        if count == 3 {
            r.cast(card)
                .modes(&[1])
                .target_player(P0)
                .resolve()
                .assert_hand_drawn(P0, 2);
            assert!(islands.iter().all(|id| !r.state().objects[id].tapped));
            assert!(r.state().objects[&astral].tapped);
        } else {
            r.cast(card).resolve().assert_hand_drawn(P0, 1);
            if count == 0 {
                assert!(!r.state().objects[&astral].tapped);
            }
        }
        assert_eq!(
            r.state().objects[&astral]
                .counters
                .get(&charge)
                .copied()
                .unwrap_or(0),
            count
        );
        assert_eq!(r.state().objects[&card].zone, Zone::Graveyard);
        assert_complete(&r, 0);
    }
}

#[test]
fn unannounced_x_baku_remains_selectable_and_produces_chosen_amount() {
    let mut s = board();
    let baku = s
        .add_creature_from_oracle(P0, "Petalmane Baku", 1, 2, BAKU)
        .id();
    let ki = CounterType::Generic("ki".into());
    s.with_counter(baku, ki.clone(), 2);
    s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.");
    let mut r = s.build();
    assert_any_one(&r, baku);
    let selections: Vec<_> = activatable_mana_source_selections(r.state(), P0)
        .into_iter()
        .filter(|x| x.source.object_id == baku)
        .collect();
    assert_eq!(selections.len(), 1);
    assert!(matches!(
        selections[0].output,
        ManaSourceOutput::DeferredColorChoice {
            quantity: ManaSourceQuantity::Variable
        }
    ));
    r.act(GameAction::ActivateManaSource {
        selection: selections[0].clone(),
    })
    .unwrap();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::PayAmountChoice {
            player: P0,
            max: 2,
            ..
        }
    ));
    r.act(GameAction::SubmitPayAmount { amount: 2 }).unwrap();
    choose_color(&mut r, ManaType::Blue);
    assert_eq!(
        r.state().objects[&baku]
            .counters
            .get(&ki)
            .copied()
            .unwrap_or(0),
        0
    );
    assert_eq!(
        r.state().players[0].mana_pool.count_color(ManaType::Blue),
        2
    );
    assert_complete(&r, 2);
}

#[test]
fn lute_grant_pays_instants_sorceries_and_generic_remainders() {
    for (name, instant, text, cost, lands, used, excess) in [
        (
            "Vision Skeins",
            true,
            "Each player draws two cards.",
            "{1}{U}",
            1,
            1,
            0,
        ),
        (
            "Sign in Blood",
            false,
            "Target player draws two cards and loses 2 life.",
            "{B}{B}",
            1,
            1,
            0,
        ),
        // Generic ranking uses one remaining land's native C after the granted UU.
        ("Divination", false, "Draw two cards.", "{2}{U}", 3, 2, 0),
    ] {
        let mut s = board();
        s.add_artifact_from_oracle(P0, "Resonating Lute", LUTE);
        // Stolen land: the grant follows controller, not owner.
        let sources: Vec<_> = (0..lands)
            .map(|_| {
                s.add_land_from_oracle(P1, "Wastes", "{T}: Add {C}.")
                    .controlled_by(P0)
                    .id()
            })
            .collect();
        let enemy_land = s
            .add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.")
            .controlled_by(P1)
            .id();
        let card = spell(&mut s, name, instant, text, cost);
        let mut r = s.build();
        evaluate_layers(r.state_mut());
        for &source in &sources {
            assert_any_one(&r, source);
        }
        assert!(r.state().objects[&enemy_land]
            .abilities
            .iter()
            .all(|a| !matches!(
                &*a.effect,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor { .. },
                    ..
                }
            )));
        let out = if name == "Sign in Blood" {
            r.cast(card).target_player(P0).resolve()
        } else {
            r.cast(card).resolve()
        };
        out.assert_hand_drawn(P0, 2);
        if name == "Vision Skeins" {
            out.assert_hand_drawn(P1, 2);
        }
        if name == "Sign in Blood" {
            out.assert_life_delta(P0, -2);
        }
        out.assert_zone(&[card], Zone::Graveyard);
        assert_eq!(
            sources
                .iter()
                .filter(|id| r.state().objects[id].tapped)
                .count(),
            used
        );
        assert!(r.state().players[0]
            .mana_pool
            .units()
            .all(|u| !u.restrictions.is_empty()));
        assert_complete(&r, excess);
    }
}

#[test]
fn concrete_lute_land_action_retains_restrictions_and_rejects_stale_reuse() {
    for reject_creature in [false, true] {
        let mut s = board();
        s.add_artifact_from_oracle(P0, "Resonating Lute", LUTE);
        let land = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
        let skeins = spell(
            &mut s,
            "Vision Skeins",
            true,
            "Each player draws two cards.",
            "{1}{U}",
        );
        let bears = s
            .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
            .id();
        let mut r = s.build();
        evaluate_layers(r.state_mut());
        let index = assert_any_one(&r, land);
        let color = if reject_creature {
            ManaType::Green
        } else {
            ManaType::Blue
        };
        let (_, _, grouped) = legal_actions_full(r.state());
        let action = grouped
            .get(&land)
            .expect("land has public grouped actions")
            .iter()
            .find(|a| {
                matches!(a,
                    GameAction::TapLandForMana { selection } if selection.source.object_id == land
                        && selection.ability_index == Some(index) && selection.mana_type == color
                )
            })
            .expect("public concrete action for granted two-unit ability")
            .clone();
        r.act(action.clone()).unwrap();
        assert!(r.state().objects[&land].tapped);
        assert_complete(&r, 2);
        assert_eq!(r.state().players[0].mana_pool.count_color(color), 2);
        assert!(r.state().players[0]
            .mana_pool
            .units()
            .all(|u| u.source_id == land && !u.restrictions.is_empty()));
        assert!(
            r.act(action).is_err(),
            "a tapped incarnation cannot activate again"
        );
        if reject_creature {
            let error = r
                .cast(bears)
                .try_resolve()
                .err()
                .expect("restricted mana cannot pay for a creature");
            assert!(error.to_string().contains("mana"), "{error}");
            assert_eq!(r.state().objects[&bears].zone, Zone::Hand);
            assert_eq!(r.state().players[0].mana_pool.total(), 2);
            assert_complete(&r, 2);
        } else {
            let out = r.cast(skeins).resolve();
            out.assert_hand_drawn(P0, 2);
            out.assert_hand_drawn(P1, 2);
            out.assert_zone(&[skeins], Zone::Graveyard);
            assert_complete(&r, 0);
        }
    }
}

#[test]
fn lute_mana_cannot_pay_mind_stone_draw_but_unrestricted_mana_can() {
    // CR 106.6: Lute's "only to cast instant and sorcery spells" excludes
    // Mind Stone's "{1}, {T}, Sacrifice this artifact: Draw a card."
    for color in [ManaType::Blue, ManaType::Colorless] {
        let mut s = board();
        s.add_artifact_from_oracle(P0, "Resonating Lute", LUTE);
        let land = s.add_land_from_oracle(P0, "Wastes", "{T}: Add {C}.").id();
        let stone = s
            .add_artifact_from_oracle(P0, "Mind Stone", MIND_STONE)
            .id();
        let mut r = s.build();
        evaluate_layers(r.state_mut());
        let granted_index = assert_any_one(&r, land);
        let (_, _, grouped) = legal_actions_full(r.state());
        let action = grouped[&land]
            .iter()
            .find(|action| {
                matches!(action,
                    GameAction::TapLandForMana { selection }
                        if selection.mana_type == color
                            && (selection.ability_index == Some(granted_index))
                                == (color == ManaType::Blue)
                )
            })
            .expect("both the native and Lute-granted mana actions must be offered")
            .clone();
        r.act(action).unwrap();
        let units: Vec<_> = r.state().players[0].mana_pool.units().collect();
        assert_eq!(units.len(), if color == ManaType::Blue { 2 } else { 1 });
        assert!(units
            .iter()
            .all(|unit| unit.source_id == land && unit.color == color));
        assert!(r.state().objects[&land].tapped);
        assert!(!r.state().objects[&stone].tapped);
        assert_eq!(r.state().objects[&stone].zone, Zone::Battlefield);
        if color == ManaType::Blue {
            assert!(units.iter().all(|unit| !unit.restrictions.is_empty()));
            let hand_before = r.state().players[0].hand.clone();
            let error = r
                .act(GameAction::ActivateAbility {
                    source_id: stone,
                    ability_index: 1,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect_err("spell-only mana cannot pay an activated ability cost");
            assert!(error.to_string().contains("mana"), "{error}");
            assert_eq!(
                r.state().players[0].mana_pool.units().collect::<Vec<_>>(),
                units
            );
            assert_eq!(r.state().players[0].hand, hand_before);
            assert_eq!(r.state().objects[&stone].zone, Zone::Battlefield);
            assert!(!r.state().objects[&stone].tapped);
            assert!(r.state().objects[&land].tapped);
            assert_complete(&r, 2);
        } else {
            assert!(units.iter().all(|unit| unit.restrictions.is_empty()));
            let out = r.activate(stone, 1).resolve();
            out.assert_hand_drawn(P0, 1);
            out.assert_zone(&[stone], Zone::Graveyard);
            assert_complete(&r, 0);
        }
    }
}

#[test]
fn one_lotus_cannot_split_its_output_between_helix_colors() {
    for independent_sources in [false, true] {
        let mut s = board();
        let lotus = s.add_artifact_from_oracle(P0, "Gilded Lotus", LOTUS).id();
        if independent_sources {
            s.add_basic_land(P0, ManaColor::Red);
            s.add_basic_land(P0, ManaColor::White);
        }
        let helix = spell(
            &mut s,
            "Lightning Helix",
            true,
            "Lightning Helix deals 3 damage to any target and you gain 3 life.",
            "{R}{W}",
        );
        let skeins = spell(
            &mut s,
            "Vision Skeins",
            true,
            "Each player draws two cards.",
            "{1}{U}",
        );
        let mut r = s.build();
        assert_any_one(&r, lotus);
        if independent_sources {
            let out = r.cast(helix).target_player(P1).resolve();
            out.assert_life_delta(P1, -3);
            out.assert_life_delta(P0, 3);
            out.assert_zone(&[helix], Zone::Graveyard);
            assert!(r.state().stack.is_empty());
            assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
        } else {
            let error = r
                .cast(helix)
                .target_player(P1)
                .try_resolve()
                .err()
                .expect("one same-color bundle cannot pay RW");
            assert!(error.to_string().contains("mana"), "{error}");
            assert_eq!(r.state().objects[&helix].zone, Zone::Hand);
            assert!(!r.state().objects[&lotus].tapped);
            assert!(legal_actions(r.state()).contains(&GameAction::CancelCast));
            r.act(GameAction::CancelCast).unwrap();
            assert_complete(&r, 0);
            let out = r.cast(skeins).resolve();
            out.assert_hand_drawn(P0, 2);
            out.assert_hand_drawn(P1, 2);
            assert_complete(&r, 1);
        }
    }
}

#[test]
fn goldspan_deferred_grant_chooses_once_and_completes_sacrificial_payment() {
    for back_first in [false, true] {
        let mut s = board();
        s.add_creature(P0, "Goldspan Dragon", 4, 4)
            .from_oracle_text_with_keywords(&["Flying", "Haste"], GOLDSPAN);
        // CR 111.10a: standard Treasure subtype and its printed token ability.
        let treasures: Vec<_> = (0..2)
            .map(|_| {
                s.add_artifact_from_oracle(
                    P0,
                    "Treasure",
                    "{T}, Sacrifice this token: Add one mana of any color.",
                )
                .with_subtypes(vec!["Treasure"])
                .id()
            })
            .collect();
        let skeins = spell(
            &mut s,
            "Vision Skeins",
            true,
            "Each player draws two cards.",
            "{1}{U}",
        );
        let mut r = s.build();
        for id in &treasures {
            r.state_mut().objects.get_mut(id).unwrap().is_token = true;
        }
        evaluate_layers(r.state_mut());
        let card_id = r.state().objects[&skeins].card_id;
        r.act(GameAction::CastSpell {
            object_id: skeins,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
        })
        .unwrap();
        let options = match &r.state().waiting_for {
            WaitingFor::ManaSourceSelection {
                player, options, ..
            } => {
                assert_eq!(*player, P0);
                options.clone()
            }
            other => panic!("expected sacrifice consent, got {other:?}"),
        };
        let selected: Vec<_> = options
            .iter()
            .filter(|x| {
                x.source.object_id == treasures[0]
                    && matches!(
                        x.output,
                        ManaSourceOutput::DeferredColorChoice {
                            quantity: ManaSourceQuantity::Fixed(2)
                        }
                    )
            })
            .collect();
        assert_eq!(
            selected.len(),
            1,
            "one deferred capability, not five color rows"
        );
        let selection = (*selected[0]).clone();
        assert!(selection.atomic_combination.is_none());
        let native = options
            .iter()
            .find(|x| {
                x.source.object_id == treasures[0]
                    && matches!(
                        x.output,
                        ManaSourceOutput::DeferredColorChoice {
                            quantity: ManaSourceQuantity::Fixed(1)
                        }
                    )
            })
            .expect("native ability is still distinct");
        assert_ne!(native.ability_index, selection.ability_index);
        if back_first {
            r.act(GameAction::BackToManaPayment).unwrap();
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::ManaPayment { .. }
            ));
            assert_eq!(r.state().players[0].mana_pool.total(), 0);
            assert!(treasures
                .iter()
                .all(|id| r.state().objects[id].zone == Zone::Battlefield));
            // The restored payment window exposes ActivateAbility, not the
            // consent-window-only ActivateManaSource action.
            let action = legal_actions(r.state()).into_iter().find(|action| matches!(action,
                GameAction::ActivateAbility { source_id, ability_index, .. }
                    if *source_id == treasures[0] && Some(*ability_index) == selection.ability_index
            )).expect("restored payment offers the same granted ability");
            r.act(action).unwrap();
        } else {
            r.act(GameAction::ActivateManaSource { selection }).unwrap();
        }
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ));
        assert!(r
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Colorless),
                count: 1
            })
            .is_err());
        choose_color(&mut r, ManaType::Blue);
        if matches!(r.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            r.act(GameAction::PassPriority).unwrap();
        }
        assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(r.state().stack.len(), 1);
        let hand_before = [
            r.state().players[0].hand.len(),
            r.state().players[1].hand.len(),
        ];
        r.pass_both_players();
        for (i, before) in hand_before.into_iter().enumerate() {
            assert_eq!(r.state().players[i].hand.len(), before + 2);
        }
        assert_eq!(r.state().objects[&skeins].zone, Zone::Graveyard);
        assert!(r
            .state()
            .objects
            .get(&treasures[0])
            .is_none_or(|o| o.zone != Zone::Battlefield));
        assert_eq!(r.state().objects[&treasures[1]].zone, Zone::Battlefield);
        assert_complete(&r, 0);
    }
}

#[test]
fn nyx_choice_dependent_count_uses_this_activation_color() {
    // CR 700.5 + CR 608.2d: devotion reads this activation's chosen color.
    for (color, expected) in [
        (ManaType::Green, 3),
        (ManaType::Blue, 1),
        (ManaType::Red, 0),
    ] {
        let mut s = board();
        let lotus = s.add_artifact_from_oracle(P0, "Nyx Lotus", NYX).id();
        s.add_creature_from_oracle(P0, "Leatherback Baloth", 4, 5, "")
            .with_mana_cost(parse_mtgjson_mana_cost("{G}{G}{G}"));
        s.add_creature_from_oracle(P0, "Merfolk of the Pearl Trident", 1, 1, "")
            .with_mana_cost(parse_mtgjson_mana_cost("{U}"));
        let mut r = s.build();
        r.state_mut()
            .objects
            .get_mut(&lotus)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Blue));
        let index = assert_any_one(&r, lotus);
        r.act(GameAction::ActivateAbility {
            source_id: lotus,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
        choose_color(&mut r, color);
        assert_eq!(r.state().players[0].mana_pool.count_color(color), expected);
        assert_eq!(
            r.state().objects[&lotus].chosen_color(),
            Some(ManaColor::Blue)
        );
        assert!(r.state().objects[&lotus].tapped);
        assert_complete(&r, expected);
    }
}
