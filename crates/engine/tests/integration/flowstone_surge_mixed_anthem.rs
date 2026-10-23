//! Flowstone Surge — "Creatures you control get +1/-1." (an enchantment)
//!
//! Regression coverage for the continuous static P/T anthem building block with
//! a MIXED modification — power up, toughness down in the same clause. Axes:
//!   - **mixed +1/-1** — one anthem applies both a positive and a negative
//!     modifier (CR 613.4c),
//!   - **"you control"** — opponents' creatures are excluded (CR 109.4),
//!   - **lifetime** — the effect ends when the source leaves (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_nom::primitives::parse_mana_cost;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use nom::{combinator::all_consuming, Parser};

const FLOWSTONE_SURGE: &str = "Creatures you control get +1/-1.";

const U22_RETURN: &str = "During your turn, each instant and sorcery card in your graveyard has flashback. Its flashback cost is equal to its mana cost.";
const U22_DOOM: &str = "Destroy target nonblack creature.";
const U22_DIVINATION: &str = "Draw two cards.";
const U22_COUNTER: &str = "Counter target spell.";
const U22_VISION: &str = "Suspend 4—{U} (Rather than cast this card from your hand, pay {U} and exile it with four time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)\nTarget player draws three cards.";
const U22_PACT: &str = "Create a 4/4 red Giant creature token.\nAt the beginning of your next upkeep, pay {4}{R}. If you don't, you lose the game.";
const U22_SPHERE: &str = "Spells cost {1} more to cast.";
const U22_THINK: &str = "Draw a card.\nFlashback {2}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const U22_FIREBOLT: &str = "Firebolt deals 2 damage to any target.\nFlashback {4}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const U22_SCREECH: &str = "Create two 1/1 white Bird creature tokens with flying.\nFlashback—Tap three untapped white creatures you control. (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const U22_ANALYSIS: &str = "Target player draws two cards.\nFlashback—{1}{U}, Pay 3 life. (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const U22_INVOKE: &str = "You may cast up to two instant and/or sorcery spells with total mana value 6 or less from your graveyard and/or hand without paying their mana costs. If those spells would be put into your graveyard, exile them instead. Exile Invoke Calamity.";
const U22_DISPERSE: &str = "Return target nonland permanent to its owner's hand.";
const U22_DONATE: &str = "Target player gains control of target permanent you control.";
const U22_LIER: &str = "Spells can't be countered.\nEach instant and sorcery card in your graveyard has flashback. The flashback cost is equal to that card's mana cost.";
const U22_MURDER: &str = "Destroy target creature.";
const U22_DEATHMARK: &str = "Destroy target green or white creature.";
const U22_LIGHT: &str = "Light of Judgment deals 6 damage to target creature. Destroy up to one Equipment attached to that creature.";
const U22_THEFT: &str =
    "Attach target Equipment to target creature. (Control of the Equipment doesn't change.)";
const U22_AXE: &str = "Indestructible (Effects that say \"destroy\" don't destroy this Equipment.)\nEquipped creature gets +2/+0.\nEquip {2}";
const U22_OTARIA_ATOMIC: &str = r##"{"name":"Otaria","text":"Instant and sorcery cards in graveyards have flashback. The flashback cost is equal to the card's mana cost. (Its owner may cast the card from their graveyard for its mana cost. Then they exile it.)\nWhenever chaos ensues, take an extra turn after this one.","manaValue":0.0,"layout":"planar","types":["Plane"],"type":"Plane — Dominaria","subtypes":["Dominaria"],"supertypes":[],"colors":[],"colorIdentity":[],"identifiers":{"scryfallOracleId":"29e3371d-aa41-46a9-921a-96565be47eff"},"rulings":[{"date":"2009-10-01","text":"A face-up plane card that's turned face down becomes a new object with no relation to its previous existence. In particular, it loses all counters it may have had."},{"date":"2009-10-01","text":"A plane card is treated as if its text box included \"When you roll {PW}, put this card on the bottom of its owner's planar deck face down, then move the top card of your planar deck off that planar deck and turn it face up.\" This is called the \"planeswalking ability.\""},{"date":"2009-10-01","text":"If an ability of a plane refers to \"you,\" it's referring to whoever the plane's controller is at the time, not to the player that started the game with that plane card in their deck. Many abilities of plane cards affect all players, while many others affect only the planar controller, so read each ability carefully."},{"date":"2009-10-01","text":"If you roll {CHAOS} multiple times in the same turn, you'll take that many extra turns after this one."},{"date":"2009-10-01","text":"The controller of a face-up plane card is the player designated as the \"planar controller.\" Normally, the planar controller is whoever the active player is. However, if the current planar controller would leave the game, instead the next player in turn order that wouldn't leave the game becomes the planar controller, then the old planar controller leaves the game. The new planar controller retains that designation until they leave the game or a different player becomes the active player, whichever comes first."},{"date":"2021-03-19","text":"\"Flashback [cost]\" means \"You may cast this card from your graveyard by paying [cost] rather than paying its mana cost\" and \"If the flashback cost was paid, exile this card instead of putting it anywhere else any time it would leave the stack.\""},{"date":"2021-03-19","text":"A spell cast using flashback will always be exiled afterward, whether it resolves, is countered, or leaves the stack in some other way."},{"date":"2021-03-19","text":"If a card has multiple instances of flashback, you may choose any of its flashback costs to pay."},{"date":"2021-03-19","text":"If a card with flashback is put into your graveyard during your turn, you can cast it if it's legal to do so before any other player can take any actions."},{"date":"2021-03-19","text":"If a card with no mana cost gains flashback, it has no flashback cost. It can't be cast this way."},{"date":"2021-03-19","text":"If a split card gains flashback, you pay only the cost of the half you're casting."},{"date":"2021-03-19","text":"If you cast a spell with flashback, you can't pay any alternative costs such as overload costs. You can pay additional costs such as kicker costs. If the spell has any mandatory additional costs, you must pay those to cast the spell with flashback."},{"date":"2021-03-19","text":"If you cast an instant or sorcery with {X} in its mana cost this way, you still choose the value of X as part of casting the spell and pay that cost."},{"date":"2021-03-19","text":"To determine the total cost of a spell, start with the mana cost or alternative cost (such as a flashback cost) you're paying, add any cost increases, then apply any cost reductions. The mana value of the spell is determined only by its mana cost, no matter what the total cost to cast the spell was."},{"date":"2021-03-19","text":"You can cast a spell using flashback even if it was somehow put into your graveyard without having been cast."},{"date":"2021-03-19","text":"You must still follow any timing restrictions and permissions, including those based on the card's type. For instance, you can cast a sorcery using flashback only when you could normally cast a sorcery."}]}"##;
const U22_TAZEEM_ATOMIC: &str = r##"{"name":"Tazeem","text":"Creatures can't block.\nWhenever chaos ensues, draw a card for each land you control.","manaValue":0.0,"layout":"planar","types":["Plane"],"type":"Plane — Zendikar","subtypes":["Zendikar"],"supertypes":[],"colors":[],"colorIdentity":[],"identifiers":{"scryfallOracleId":"97f708de-9271-4137-a9fe-dda25b922900"},"rulings":[{"date":"2009-10-01","text":"A face-up plane card that's turned face down becomes a new object with no relation to its previous existence. In particular, it loses all counters it may have had."},{"date":"2009-10-01","text":"A plane card is treated as if its text box included \"When you roll {PW}, put this card on the bottom of its owner's planar deck face down, then move the top card of your planar deck off that planar deck and turn it face up.\" This is called the \"planeswalking ability.\""},{"date":"2009-10-01","text":"If an ability of a plane refers to \"you,\" it's referring to whoever the plane's controller is at the time, not to the player that started the game with that plane card in their deck. Many abilities of plane cards affect all players, while many others affect only the planar controller, so read each ability carefully."},{"date":"2009-10-01","text":"The controller of a face-up plane card is the player designated as the \"planar controller.\" Normally, the planar controller is whoever the active player is. However, if the current planar controller would leave the game, instead the next player in turn order that wouldn't leave the game becomes the planar controller, then the old planar controller leaves the game. The new planar controller retains that designation until they leave the game or a different player becomes the active player, whichever comes first."}]}"##;

fn u22_plane_face(name: &str) -> engine::types::card::CardFace {
    let card: engine::database::mtgjson::AtomicCard = serde_json::from_str(match name {
        "Otaria" => U22_OTARIA_ATOMIC,
        "Tazeem" => U22_TAZEEM_ATOMIC,
        _ => panic!("printed planar fixture"),
    })
    .unwrap();
    assert_eq!(card.layout, "planar");
    assert_eq!(card.types, vec!["Plane"]);
    assert_eq!(
        card.subtypes,
        vec![if name == "Otaria" {
            "Dominaria"
        } else {
            "Zendikar"
        }]
    );
    assert_eq!(card.mana_cost, None);
    let face = engine::database::synthesis::build_oracle_face(
        &card,
        card.identifiers.scryfall_oracle_id.clone(),
    );
    assert_eq!(face.oracle_text.as_deref(), card.text.as_deref());
    assert_eq!(face.mana_cost, ManaCost::NoCost);
    assert!(face
        .card_type
        .core_types
        .contains(&engine::types::card_type::CoreType::Plane));
    assert_eq!(face.card_type.subtypes, card.subtypes);
    assert_eq!(face.scryfall_oracle_id, card.identifiers.scryfall_oracle_id);
    assert!(face.abilities.is_empty());
    assert_eq!(face.static_abilities.len(), 1);
    assert_eq!(face.triggers.len(), 1);
    assert_eq!(face.static_abilities[0].active_zones, vec![Zone::Command]);
    assert_eq!(face.triggers[0].trigger_zones, vec![Zone::Command]);
    let chaos = face.triggers[0].execute.as_deref().unwrap();
    if name == "Otaria" {
        assert!(matches!(
            *chaos.effect,
            engine::types::ability::Effect::ExtraTurn { .. }
        ));
    } else {
        assert_eq!(
            face.static_abilities[0].mode,
            engine::types::statics::StaticMode::CantBlock
        );
        let engine::types::ability::Effect::Draw {
            count:
                engine::types::ability::QuantityExpr::Ref {
                    qty: engine::types::ability::QuantityRef::ObjectCount { filter, .. },
                },
            target,
        } = &*chaos.effect
        else {
            panic!("Tazeem's complete land-count draw")
        };
        assert_eq!(*target, engine::types::ability::TargetFilter::Controller);
        let engine::types::ability::TargetFilter::Typed(lands) = filter else {
            panic!("typed land population")
        };
        assert!(lands
            .type_filters
            .contains(&engine::types::ability::TypeFilter::Land));
        assert_eq!(
            lands.controller,
            Some(engine::types::ability::ControllerRef::You)
        );
    }
    let whole = serde_json::to_string(&serde_json::json!({name.to_lowercase(): face})).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &whole).unwrap();
    for database in [
        engine::database::CardDatabase::from_json_str(&whole).unwrap(),
        engine::database::CardDatabase::from_export(file.path()).unwrap(),
    ] {
        assert_eq!(database.get_face_by_name(name), Some(&face));
    }
    face
}

fn u22_starting_otaria(runner: &mut GameRunner) -> (ObjectId, ObjectId) {
    let otaria = u22_plane_face("Otaria");
    let tazeem = u22_plane_face("Tazeem");
    let state = runner.state_mut();
    state.format_config.format = engine::types::format::GameFormat::Planechase;
    let mut ids = Vec::new();
    for (index, face) in [&otaria, &tazeem].into_iter().enumerate() {
        let id = engine::game::zones::create_object(
            state,
            engine::types::identifiers::CardId(22_100 + index as u64),
            P0,
            face.name.clone(),
            Zone::Command,
        );
        let object = state.objects.get_mut(&id).unwrap();
        engine::game::printed_cards::apply_card_face_to_object(object, face);
        object.face_down = true;
        state.planar_deck.push_back(id);
        ids.push(id);
    }
    // CR 901.5 + CR 311.4: reveal the real starting plane; only active membership functions.
    engine::game::planechase::reveal_starting_plane(state);
    assert_eq!(engine::game::planechase::active_plane(state), Some(ids[0]));
    assert!(state.command_zone.contains(&ids[0]) && state.planar_deck.contains(&ids[1]));
    assert_eq!(state.objects[&ids[0]].zone, Zone::Command);
    assert_eq!(state.planar_controller, Some(P0));
    assert!(!state.objects[&ids[0]].face_down);
    (ids[0], ids[1])
}

fn u22_paid_return(runner: &mut GameRunner, source: ObjectId) {
    assert!(runner.state().objects[&source]
        .static_definitions
        .iter_unchecked()
        .any(|entry| entry.description.as_deref() == Some(U22_RETURN)));
    assert_eq!(
        runner.state().objects[&source].mana_cost,
        complete_publication_cost("{4}{R}{R}")
    );
    complete_publication_fund(runner, P0, 4, &[ManaType::Red, ManaType::Red]);
    let committed = runner.cast(source).commit();
    assert_eq!(committed.state().objects[&source].zone, Zone::Stack);
    assert_eq!(committed.state().players[0].mana_pool.total(), 0);
    committed
        .resolve()
        .assert_zone(&[source], Zone::Battlefield);
    assert_continuous_complete(runner.state());
}

#[test]
fn u6_return_paid_graveyard_instant_and_sorcery_resolve_and_exile() {
    use engine::game::casting::{can_cast_object_now, current_casting_variant_choice_options};
    use engine::types::game_state::{CastPaymentMode, CastingVariant, StackEntryKind};
    for sorcery in [false, true] {
        for countered in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_library_top(
                P0,
                &["First draw", "Second draw", "Spare draw", "More draw"],
            );
            let source = scenario
                .add_spell_to_hand_from_oracle(P0, "Return the Past", false, U22_RETURN)
                .as_enchantment()
                .with_mana_cost(complete_publication_cost("{4}{R}{R}"))
                .id();
            let recipient = scenario
                .add_spell_to_graveyard(
                    P0,
                    if sorcery { "Divination" } else { "Doom Blade" },
                    !sorcery,
                )
                .from_oracle_text(if sorcery { U22_DIVINATION } else { U22_DOOM })
                .with_mana_cost(complete_publication_cost(if sorcery {
                    "{2}{U}"
                } else {
                    "{1}{B}"
                }))
                .id();
            let first = scenario
                .add_creature(P1, "First legal victim", 3, 3)
                .with_color(vec![ManaColor::Green])
                .id();
            let chosen = scenario
                .add_creature(P1, "Nondefault legal victim", 4, 4)
                .with_color(vec![ManaColor::White])
                .id();
            let black = scenario
                .add_creature(P1, "Black excluded", 5, 5)
                .with_color(vec![ManaColor::Black])
                .id();
            let creature = scenario
                .add_creature_to_graveyard(P0, "Creature excluded", 2, 2)
                .id();
            let foreign = scenario
                .add_spell_to_graveyard(P1, "Doom Blade", true)
                .from_oracle_text(U22_DOOM)
                .with_mana_cost(complete_publication_cost("{1}{B}"))
                .id();
            let counter = scenario
                .add_spell_to_hand_from_oracle(P1, "Counterspell", true, U22_COUNTER)
                .with_mana_cost(complete_publication_cost("{U}{U}"))
                .id();
            let remove_source = scenario
                .add_spell_to_hand_from_oracle(P1, "Disperse", true, U22_DISPERSE)
                .with_mana_cost(complete_publication_cost("{1}{U}"))
                .id();
            let mut runner = scenario.build();
            u22_paid_return(&mut runner, source);
            assert!(!can_cast_object_now(runner.state(), P0, creature));
            assert!(!can_cast_object_now(runner.state(), P0, foreign));
            assert!(!can_cast_object_now(runner.state(), P1, foreign));
            assert!(
                current_casting_variant_choice_options(runner.state(), P0, recipient).is_empty(),
                "unfunded paired with exact paid cast below"
            );
            complete_publication_fund(
                &mut runner,
                P0,
                if sorcery { 2 } else { 1 },
                &[if sorcery {
                    ManaType::Blue
                } else {
                    ManaType::Black
                }],
            );
            let options = current_casting_variant_choice_options(runner.state(), P0, recipient);
            assert_eq!(options.len(), 1);
            assert_eq!(options[0].variant, CastingVariant::Flashback);
            assert_eq!(
                options[0].mana_cost,
                runner.state().objects[&recipient].mana_cost
            );
            assert!(can_cast_object_now(runner.state(), P0, recipient));
            let hand_before = runner.state().players[0].hand.len();
            runner
                .act(GameAction::CastSpell {
                    object_id: recipient,
                    card_id: runner.state().objects[&recipient].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            if !sorcery {
                let WaitingFor::TargetSelection {
                    player,
                    pending_cast,
                    target_slots,
                    selection,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("actual target prompt")
                };
                assert_eq!(*player, P0);
                assert_eq!(pending_cast.object_id, recipient);
                assert_eq!(target_slots.len(), 1);
                let legal = &target_slots[selection.current_slot].legal_targets;
                assert!(
                    legal.contains(&TargetRef::Object(first))
                        && legal.contains(&TargetRef::Object(chosen))
                );
                assert!(!legal.contains(&TargetRef::Object(black)));
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(chosen)),
                    })
                    .unwrap();
            }
            assert_eq!(runner.state().objects[&recipient].zone, Zone::Stack);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(runner.state().stack.len(), 1);
            assert!(matches!(
                runner.state().stack[0].kind,
                StackEntryKind::Spell {
                    casting_variant: CastingVariant::Flashback,
                    ..
                }
            ));
            complete_publication_fund(&mut runner, P1, 1, &[ManaType::Blue]);
            runner.act(GameAction::PassPriority).unwrap();
            let response = runner.cast(remove_source).target_object(source).commit();
            assert_eq!(response.state().stack.len(), 2);
            assert_eq!(response.state().stack[0].source_id, recipient);
            assert_eq!(response.state().stack[1].source_id, remove_source);
            assert_eq!(response.state().players[1].mana_pool.total(), 0);
            drop(response);
            runner.pass_both_players();
            assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&remove_source].zone, Zone::Graveyard);
            assert_eq!(
                runner.state().stack.len(),
                1,
                "the paid lower spell remains pending after its grantor leaves"
            );
            assert_eq!(runner.state().stack[0].source_id, recipient);
            assert!(matches!(
                runner.state().stack[0].kind,
                StackEntryKind::Spell {
                    casting_variant: CastingVariant::Flashback,
                    ..
                }
            ));
            if countered {
                complete_publication_fund(&mut runner, P1, 0, &[ManaType::Blue, ManaType::Blue]);
                runner.act(GameAction::PassPriority).unwrap();
                let committed = runner.cast(counter).target_object(recipient).commit();
                assert_eq!(committed.state().stack.len(), 2);
                assert_eq!(committed.state().stack[0].source_id, recipient);
                assert_eq!(committed.state().stack[1].source_id, counter);
                assert_eq!(committed.state().players[1].mana_pool.total(), 0);
                committed.resolve();
                assert_eq!(runner.state().objects[&counter].zone, Zone::Graveyard);
            } else {
                runner.advance_until_stack_empty();
            }
            assert_eq!(runner.state().objects[&recipient].zone, Zone::Exile);
            assert_eq!(
                runner.state().objects[&chosen].zone,
                if !sorcery && !countered {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            );
            assert_eq!(
                runner.state().players[0].hand.len(),
                hand_before + if sorcery && !countered { 2 } else { 0 }
            );
            assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&black].zone, Zone::Battlefield);
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn u6_derived_flashback_absent_cost_is_unpayable_but_zero_is_paid() {
    use engine::game::casting::{can_cast_object_now, current_casting_variant_choice_options};
    use engine::types::game_state::{CastPaymentMode, CastingVariant};
    use engine::types::keywords::{FlashbackCost, KeywordKind};
    for planar in [false, true] {
        for surcharge in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_library_top(
                P0,
                &[
                    "Vision draw A",
                    "Vision draw B",
                    "Vision draw C",
                    "Spare A",
                    "Spare B",
                ],
            );
            scenario.with_library_top(
                P1,
                &["Opponent draw A", "Opponent draw B", "Opponent draw C"],
            );
            let source = scenario
                .add_spell_to_hand_from_oracle(P0, "Return the Past", false, U22_RETURN)
                .as_enchantment()
                .with_mana_cost(complete_publication_cost("{4}{R}{R}"))
                .id();
            let vision = scenario
                .add_spell_to_graveyard(P0, "Ancestral Vision", false)
                .from_oracle_text(U22_VISION)
                .with_mana_cost(ManaCost::NoCost)
                .with_color(vec![ManaColor::Blue])
                .id();
            let pact = scenario
                .add_spell_to_graveyard(P0, "Pact of the Titan", true)
                .from_oracle_text(U22_PACT)
                .with_mana_cost(complete_publication_cost("{0}"))
                .with_color(vec![ManaColor::Red])
                .id();
            let sphere = scenario
                .add_artifact_to_hand_from_oracle(P0, "Sphere of Resistance", U22_SPHERE)
                .with_mana_cost(complete_publication_cost("{2}"))
                .id();
            let mut runner = scenario.build();
            if planar {
                u22_starting_otaria(&mut runner);
            } else {
                u22_paid_return(&mut runner, source);
            }
            if surcharge {
                complete_publication_fund(&mut runner, P0, 2, &[]);
                runner
                    .cast(sphere)
                    .resolve()
                    .assert_zone(&[sphere], Zone::Battlefield);
                assert_continuous_complete(runner.state());
            }
            assert_eq!(runner.state().phase, Phase::PreCombatMain);
            assert_eq!(runner.state().active_player, P0);
            assert_eq!(runner.state().objects[&vision].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&vision].owner, P0);
            assert!(runner.state().objects[&vision]
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Sorcery));
            assert!(engine::game::keywords::object_has_effective_keyword_kind(
                runner.state(),
                vision,
                KeywordKind::Flashback
            ));
            assert_eq!(
                engine::game::keywords::effective_flashback_cost(runner.state(), vision),
                Some(FlashbackCost::Mana(ManaCost::NoCost))
            );
            assert_eq!(runner.state().objects[&vision].mana_cost, ManaCost::NoCost);
            assert_eq!(runner.state().objects[&pact].mana_cost, ManaCost::zero());
            // CR 118.6 + CR 118.6a: the selected absent mana basis stays unpayable before increases.
            assert!(
                !current_casting_variant_choice_options(runner.state(), P0, vision)
                    .iter()
                    .any(|option| option.variant == CastingVariant::Flashback)
            );
            assert!(!can_cast_object_now(runner.state(), P0, vision));
            assert!(!engine::ai_support::legal_actions(runner.state()).iter().any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == vision)));
            let hand = runner.state().players[0].hand.clone();
            let life = runner.state().players[0].life;
            let mana = runner.state().players[0].mana_pool.total();
            let attempt = runner.act(GameAction::CastSpell {
                object_id: vision,
                card_id: runner.state().objects[&vision].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            });
            if attempt.is_ok() {
                let WaitingFor::TargetSelection {
                    player,
                    pending_cast,
                    target_slots,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("an admitted raw attempt must expose its real announced frame")
                };
                assert_eq!(*player, P0);
                assert_eq!(pending_cast.object_id, vision);
                assert!(target_slots
                    .iter()
                    .any(|slot| slot.legal_targets.contains(&TargetRef::Player(P0))));
                let _ = runner.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P0)),
                });
                assert_ne!(
                    runner.state().objects[&vision].zone,
                    Zone::Stack,
                    "a raw cast cannot complete payment of the absent Flashback basis"
                );
                assert!(!runner
                    .state()
                    .stack
                    .iter()
                    .any(|entry| entry.source_id == vision));
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    runner.act(GameAction::CancelCast).unwrap();
                }
            }
            assert_eq!(runner.state().players[0].hand, hand);
            assert_eq!(runner.state().players[0].life, life);
            assert_eq!(runner.state().players[0].mana_pool.total(), mana);
            assert_eq!(runner.state().objects[&vision].zone, Zone::Graveyard);
            assert_continuous_complete(runner.state());
            if surcharge {
                complete_publication_fund(&mut runner, P0, 1, &[]);
            }
            let options = current_casting_variant_choice_options(runner.state(), P0, pact);
            assert!(options
                .iter()
                .any(|option| option.variant == CastingVariant::Flashback
                    && option.mana_cost == ManaCost::generic(u32::from(surcharge))));
            let committed = runner
                .cast(pact)
                .casting_variant(CastingVariant::Flashback)
                .commit();
            assert_eq!(committed.state().objects[&pact].zone, Zone::Stack);
            assert_eq!(
                committed.state().players[0].mana_pool.total(),
                0,
                "printed zero pays exactly the real Sphere increase"
            );
            committed.resolve().assert_zone(&[pact], Zone::Exile);
            let giants = runner
                .state()
                .objects
                .values()
                .filter(|object| {
                    object.zone == Zone::Battlefield && object.is_token && object.name == "Giant"
                })
                .collect::<Vec<_>>();
            assert_eq!(giants.len(), 1);
            assert_eq!((giants[0].power, giants[0].toughness), (Some(4), Some(4)));
            assert_eq!(giants[0].controller, P0);
            assert_eq!(giants[0].color, vec![ManaColor::Red]);
            assert!(runner
                .state()
                .delayed_triggers
                .iter()
                .any(|trigger| trigger.source_id == pact));
            assert_continuous_complete(runner.state());
            // Advance actual turns to the first own upkeep carrying Pact's real duty.
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            runner.pass_both_players();
            runner.advance_to_upkeep();
            if runner.state().active_player != P0 {
                runner.advance_to_phase(Phase::PreCombatMain);
                runner.advance_to_combat();
                runner.declare_attackers(&[]).unwrap();
                runner.advance_to_end_step();
                runner.pass_both_players();
                runner.advance_to_upkeep();
            }
            assert_eq!(runner.state().active_player, P0);
            assert_eq!(runner.state().phase, Phase::Upkeep);
            assert!(runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == pact));
            complete_publication_fund(&mut runner, P0, 4, &[ManaType::Red]);
            runner.advance_until_stack_empty();
            if let WaitingFor::UnlessPayment {
                player,
                cost,
                pending_effect,
                ..
            } = &runner.state().waiting_for
            {
                assert_eq!(*player, P0);
                assert_eq!(
                    *cost,
                    engine::types::ability::AbilityCost::Mana {
                        cost: complete_publication_cost("{4}{R}")
                    }
                );
                assert_eq!(pending_effect.source_id, pact);
                runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
                runner.advance_until_stack_empty();
            }
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(runner.state().players[0].life, life);
            assert!(!runner
                .state()
                .delayed_triggers
                .iter()
                .any(|trigger| trigger.source_id == pact));
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn u6_independent_free_permission_survives_unpayable_derived_flashback() {
    use engine::game::casting::{can_cast_object_now, current_casting_variant_choice_options};
    use engine::types::ability::{
        CastingPermission, ExileGrantCostProvenance, SpellStackToGraveyardReplacement,
    };
    use engine::types::game_state::{CastOfferKind, CastingVariant, StackEntryKind};
    for planar in [false, true] {
        for accept in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_library_top(P0, &["Vision A", "Vision B", "Vision C", "Spare"]);
            let source = scenario
                .add_spell_to_hand_from_oracle(P0, "Return the Past", false, U22_RETURN)
                .as_enchantment()
                .with_mana_cost(complete_publication_cost("{4}{R}{R}"))
                .id();
            let vision = scenario
                .add_spell_to_graveyard(P0, "Ancestral Vision", false)
                .from_oracle_text(U22_VISION)
                .with_mana_cost(ManaCost::NoCost)
                .with_color(vec![ManaColor::Blue])
                .id();
            let invoke = scenario
                .add_spell_to_hand_from_oracle(P0, "Invoke Calamity", true, U22_INVOKE)
                .with_mana_cost(complete_publication_cost("{1}{R}{R}{R}{R}"))
                .id();
            let spare = scenario
                .add_spell_to_hand_from_oracle(P0, "Divination", false, U22_DIVINATION)
                .with_mana_cost(complete_publication_cost("{2}{U}"))
                .id();
            let foreign = scenario
                .add_spell_to_graveyard(P1, "Ancestral Vision", false)
                .from_oracle_text(U22_VISION)
                .with_mana_cost(ManaCost::NoCost)
                .id();
            let mut runner = scenario.build();
            if planar {
                u22_starting_otaria(&mut runner);
            } else {
                u22_paid_return(&mut runner, source);
            }
            assert!(!can_cast_object_now(runner.state(), P0, vision));
            assert!(
                !current_casting_variant_choice_options(runner.state(), P0, vision)
                    .iter()
                    .any(|option| option.variant == CastingVariant::Flashback)
            );
            let hand_before = runner.state().players[0].hand.len();
            complete_publication_fund(
                &mut runner,
                P0,
                1,
                &[ManaType::Red, ManaType::Red, ManaType::Red, ManaType::Red],
            );
            runner.cast(invoke).commit().stop_at_optional().resolve();
            if let WaitingFor::OptionalEffectChoice { player, .. } = &runner.state().waiting_for {
                assert_eq!(*player, P0);
                assert!(!runner.state().resolution_stack.is_empty());
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            let WaitingFor::CastOffer {
                player,
                kind:
                    CastOfferKind::FreeCastWindow {
                        candidates,
                        remaining_casts,
                        remaining_mv_budget,
                        graveyard_replacement,
                        ..
                    },
            } = &runner.state().waiting_for
            else {
                panic!("real Invoke free-cast window")
            };
            assert_eq!(*player, P0);
            assert_eq!(*remaining_casts, Some(2));
            assert_eq!(*remaining_mv_budget, Some(6));
            assert_eq!(
                *graveyard_replacement,
                Some(SpellStackToGraveyardReplacement::Exile)
            );
            assert!(candidates.contains(&vision) && candidates.contains(&spare));
            assert!(!candidates.contains(&foreign));
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            let frame = runner
                .state()
                .resolution_stack
                .active_ability_continuation()
                .expect("Invoke parent parked");
            assert_eq!(frame.pending.chain.source_id, invoke);
            if accept {
                runner
                    .act(GameAction::FreeCastWindowChoice {
                        selection: Some(vision),
                    })
                    .unwrap();
                let WaitingFor::TargetSelection {
                    player,
                    pending_cast,
                    target_slots,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("Vision's actual player-target announcement")
                };
                assert_eq!(*player, P0);
                assert_eq!(pending_cast.object_id, vision);
                assert_eq!(pending_cast.casting_variant, CastingVariant::Normal);
                let index = pending_cast
                    .casting_permission_index
                    .expect("selected independent index");
                assert!(
                    matches!(&runner.state().objects[&vision].casting_permissions[index.0], CastingPermission::ExileWithAltCost { cost, cost_provenance: ExileGrantCostProvenance::Alternative, resolution_cleanup: Some(_), .. } if *cost == ManaCost::zero())
                );
                assert!(target_slots
                    .iter()
                    .any(|slot| slot.legal_targets.contains(&TargetRef::Player(P0))));
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P0)),
                    })
                    .unwrap();
                assert_eq!(runner.state().objects[&vision].zone, Zone::Stack);
                let child = runner
                    .state()
                    .stack
                    .iter()
                    .find(|entry| entry.source_id == vision)
                    .unwrap();
                assert!(matches!(
                    child.kind,
                    StackEntryKind::Spell {
                        casting_variant: CastingVariant::Normal,
                        ..
                    }
                ));
                assert_eq!(runner.state().players[0].mana_pool.total(), 0);
                let WaitingFor::CastOffer {
                    player,
                    kind:
                        CastOfferKind::FreeCastWindow {
                            candidates,
                            remaining_casts,
                            remaining_mv_budget,
                            ..
                        },
                } = &runner.state().waiting_for
                else {
                    panic!("remaining window before child resolves")
                };
                assert_eq!(*player, P0);
                assert_eq!(*remaining_casts, Some(1));
                assert_eq!(*remaining_mv_budget, Some(6));
                assert!(!candidates.contains(&vision) && candidates.contains(&spare));
                assert_eq!(
                    runner.state().players[0].hand.len(),
                    hand_before - 1,
                    "child has not drawn during parent resolution"
                );
            }
            runner
                .act(GameAction::FreeCastWindowChoice { selection: None })
                .unwrap();
            assert_eq!(runner.state().objects[&invoke].zone, Zone::Exile);
            runner.advance_until_stack_empty();
            assert_eq!(
                runner.state().objects[&vision].zone,
                if accept { Zone::Exile } else { Zone::Graveyard }
            );
            assert_eq!(
                runner.state().players[0].hand.len(),
                hand_before - 1 + if accept { 3 } else { 0 }
            );
            assert_eq!(runner.state().objects[&spare].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&foreign].zone, Zone::Graveyard);
            assert!(runner.state().objects[&vision]
                .casting_permissions
                .is_empty());
            if !accept {
                assert!(!can_cast_object_now(runner.state(), P0, vision));
            }
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn ordinary_destroy_paid_murder_doom_blade_and_deathmark_keep_target_and_resolution_rules() {
    use engine::types::game_state::CastPaymentMode;
    for (name, oracle, cost, generic, colors) in [
        (
            "Murder",
            U22_MURDER,
            "{1}{B}{B}",
            1,
            vec![ManaType::Black, ManaType::Black],
        ),
        ("Doom Blade", U22_DOOM, "{1}{B}", 1, vec![ManaType::Black]),
        ("Deathmark", U22_DEATHMARK, "{B}", 0, vec![ManaType::Black]),
    ] {
        for response in 0..3 {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let spell = scenario
                .add_spell_to_hand_from_oracle(P0, name, name != "Deathmark", oracle)
                .with_mana_cost(complete_publication_cost(cost))
                .id();
            let green = scenario
                .add_creature(P1, "Green legal victim", 2, 3)
                .with_color(vec![ManaColor::Green])
                .id();
            let white = scenario
                .add_creature(P1, "White legal victim", 3, 4)
                .with_color(vec![ManaColor::White])
                .id();
            let chosen = scenario
                .add_creature(P1, "Green and white nondefault victim", 4, 5)
                .with_color(vec![ManaColor::Green, ManaColor::White])
                .id();
            let black = scenario
                .add_creature(P1, "Black target discriminator", 5, 6)
                .with_color(vec![ManaColor::Black])
                .id();
            let artifact = scenario
                .add_artifact_from_oracle(
                    P1,
                    "Bonesplitter",
                    "Equipped creature gets +2/+0.\nEquip {1}",
                )
                .with_subtypes(vec!["Equipment"])
                .with_mana_cost(complete_publication_cost("{1}"))
                .id();
            let bounce = scenario
                .add_spell_to_hand_from_oracle(P1, "Disperse", true, U22_DISPERSE)
                .with_mana_cost(complete_publication_cost("{1}{U}"))
                .id();
            let protect = scenario.add_spell_to_hand_from_oracle(P1, "Make a Stand", true,
                "Creatures you control get +1/+0 and gain indestructible until end of turn. (Damage and effects that say \"destroy\" don't destroy them.)")
                .with_mana_cost(complete_publication_cost("{2}{W}")).id();
            let mut runner = scenario.build();
            complete_publication_fund(&mut runner, P0, generic, &colors);
            runner
                .act(GameAction::CastSpell {
                    object_id: spell,
                    card_id: runner.state().objects[&spell].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            let WaitingFor::TargetSelection {
                player,
                pending_cast,
                target_slots,
                selection,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("paid removal must announce its creature target")
            };
            assert_eq!(*player, P0);
            assert_eq!(pending_cast.object_id, spell);
            assert_eq!(target_slots.len(), 1);
            let legal = &target_slots[selection.current_slot].legal_targets;
            for object in [green, white, chosen] {
                assert!(legal.contains(&TargetRef::Object(object)));
            }
            assert_eq!(
                legal
                    .iter()
                    .filter(|target| **target == TargetRef::Object(chosen))
                    .count(),
                1,
                "green-or-white is a union"
            );
            assert_eq!(legal.contains(&TargetRef::Object(black)), name == "Murder");
            assert!(!legal.contains(&TargetRef::Object(artifact)));
            assert!(runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(artifact))
                })
                .is_err());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ));
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(chosen)),
                })
                .unwrap();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(runner.state().stack.len(), 1);
            if response != 0 {
                runner.act(GameAction::PassPriority).unwrap();
                if response == 1 {
                    complete_publication_fund(&mut runner, P1, 1, &[ManaType::Blue]);
                    let paid = runner.cast(bounce).target_object(chosen).commit();
                    assert_eq!(paid.state().stack.len(), 2);
                    assert_eq!(paid.state().players[1].mana_pool.total(), 0);
                    paid.resolve();
                    assert_eq!(runner.state().objects[&bounce].zone, Zone::Graveyard);
                } else {
                    complete_publication_fund(&mut runner, P1, 2, &[ManaType::White]);
                    let paid = runner.cast(protect).commit();
                    assert_eq!(paid.state().stack.len(), 2);
                    assert_eq!(paid.state().players[1].mana_pool.total(), 0);
                    paid.resolve();
                    assert!(runner.state().objects[&chosen]
                        .keywords
                        .contains(&Keyword::Indestructible));
                    assert_eq!(runner.state().objects[&protect].zone, Zone::Graveyard);
                }
            } else {
                runner.advance_until_stack_empty();
            }
            assert_eq!(
                runner.state().objects[&chosen].zone,
                match response {
                    0 => Zone::Graveyard,
                    1 => Zone::Hand,
                    _ => Zone::Battlefield,
                }
            );
            for object in [green, white, black, artifact] {
                assert_eq!(runner.state().objects[&object].zone, Zone::Battlefield);
            }
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn ordinary_destroy_funding_color_and_no_target_failures_pair_with_paid_casts() {
    use engine::game::casting::can_cast_object_now;
    use engine::types::game_state::CastPaymentMode;
    for (name, text, cost, generic, blacks) in [
        ("Murder", U22_MURDER, "{1}{B}{B}", 1, 2),
        ("Doom Blade", U22_DOOM, "{1}{B}", 1, 1),
        ("Deathmark", U22_DEATHMARK, "{B}", 0, 1),
    ] {
        for case in 0..5 {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let spell = scenario
                .add_spell_to_hand_from_oracle(P0, name, name != "Deathmark", text)
                .with_mana_cost(complete_publication_cost(cost))
                .id();
            let victim = if case == 3 && name == "Murder" {
                None
            } else {
                Some(
                    scenario
                        .add_creature(P1, "Removal eligibility discriminator", 3, 4)
                        .with_color(vec![if case == 3 {
                            ManaColor::Black
                        } else {
                            ManaColor::White
                        }])
                        .id(),
                )
            };
            scenario
                .add_artifact_from_oracle(
                    P1,
                    "Bonesplitter",
                    "Equipped creature gets +2/+0.\nEquip {1}",
                )
                .with_subtypes(vec!["Equipment"])
                .with_mana_cost(complete_publication_cost("{1}"));
            let mut runner = scenario.build();
            match case {
                0 => {}
                1 => complete_publication_fund(
                    &mut runner,
                    P0,
                    generic,
                    &vec![ManaType::Blue; blacks],
                ),
                2 => complete_publication_fund(
                    &mut runner,
                    P0,
                    0,
                    &vec![ManaType::Black; if generic == 0 { blacks - 1 } else { blacks }],
                ),
                _ => complete_publication_fund(
                    &mut runner,
                    P0,
                    generic,
                    &vec![ManaType::Black; blacks],
                ),
            }
            let mana = runner.state().players[0].mana_pool.total();
            assert_eq!(can_cast_object_now(runner.state(), P0, spell), case == 4);
            if case == 4 {
                runner.cast(spell).target_object(victim.unwrap()).resolve();
                assert_eq!(
                    runner.state().objects[&victim.unwrap()].zone,
                    Zone::Graveyard
                );
                assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            } else {
                let attempt = runner.act(GameAction::CastSpell {
                    object_id: spell,
                    card_id: runner.state().objects[&spell].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                });
                if attempt.is_ok() {
                    let WaitingFor::TargetSelection {
                        player,
                        pending_cast,
                        ..
                    } = &runner.state().waiting_for
                    else {
                        panic!("real uncommitted removal announcement")
                    };
                    assert_eq!(*player, P0);
                    assert_eq!(pending_cast.object_id, spell);
                    runner.act(GameAction::CancelCast).unwrap();
                }
                assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
                assert_eq!(runner.state().players[0].mana_pool.total(), mana);
                if let Some(victim) = victim {
                    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
                }
            }
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn ordinary_light_paid_resolution_choice_uses_current_equipment_and_public_responses() {
    use engine::game::effects::attach::attach_to;
    use engine::types::game_state::CastPaymentMode;
    for case in 0..8 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let light = scenario
            .add_spell_to_hand_from_oracle(P0, "Light of Judgment", true, U22_LIGHT)
            .with_mana_cost(complete_publication_cost("{4}{R}"))
            .id();
        let other = scenario.add_creature(P1, "Another creature", 2, 9).id();
        let victim = scenario
            .add_creature(
                P1,
                "Declared nondefault creature",
                2,
                if case == 4 { 3 } else { 9 },
            )
            .id();
        let first = scenario
            .add_artifact_from_oracle(
                P1,
                "Bonesplitter",
                "Equipped creature gets +2/+0.\nEquip {1}",
            )
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(complete_publication_cost("{1}"))
            .id();
        let second = scenario.add_artifact_from_oracle(P0, "Short Sword", "Equipped creature gets +1/+1.\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)")
            .with_subtypes(vec!["Equipment"]).with_mana_cost(complete_publication_cost("{1}")).id();
        let axe = scenario
            .add_artifact_from_oracle(P1, "Darksteel Axe", U22_AXE)
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(complete_publication_cost("{1}"))
            .id();
        let unattached = scenario
            .add_artifact_from_oracle(
                P0,
                "Bonesplitter",
                "Equipped creature gets +2/+0.\nEquip {1}",
            )
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(complete_publication_cost("{1}"))
            .id();
        let aura = scenario
            .add_enchantment_from_oracle(
                P1,
                "Holy Strength",
                "Enchant creature\nEnchanted creature gets +1/+2.",
            )
            .with_subtypes(vec!["Aura"])
            .with_mana_cost(complete_publication_cost("{W}"))
            .id();
        let theft = scenario
            .add_spell_to_hand_from_oracle(P1, "Magnetic Theft", true, U22_THEFT)
            .with_mana_cost(complete_publication_cost("{R}"))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(P1, "Disperse", true, U22_DISPERSE)
            .with_mana_cost(complete_publication_cost("{1}{U}"))
            .id();
        let mut runner = scenario.build();
        // Initial attachment setup precedes the public cast and every response.
        attach_to(runner.state_mut(), aura, victim);
        if case != 2 {
            attach_to(runner.state_mut(), first, victim);
            attach_to(runner.state_mut(), second, victim);
        }
        attach_to(
            runner.state_mut(),
            axe,
            if case == 5 { victim } else { other },
        );
        complete_publication_fund(&mut runner, P0, 4, &[ManaType::Red]);
        runner
            .act(GameAction::CastSpell {
                object_id: light,
                card_id: runner.state().objects[&light].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            pending_cast,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Light's actual declared-creature prompt")
        };
        assert_eq!(*player, P0);
        assert_eq!(pending_cast.object_id, light);
        assert_eq!(target_slots.len(), 1);
        let legal = &target_slots[selection.current_slot].legal_targets;
        assert!(
            legal.contains(&TargetRef::Object(victim)) && legal.contains(&TargetRef::Object(other))
        );
        for object in [first, second, axe, aura] {
            assert!(!legal.contains(&TargetRef::Object(object)));
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(victim)),
            })
            .unwrap();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().objects[&light].zone, Zone::Stack);
        if case == 3 || case == 6 || case == 7 {
            runner.act(GameAction::PassPriority).unwrap();
            if case == 3 || case == 7 {
                complete_publication_fund(&mut runner, P1, 0, &[ManaType::Red]);
                let (moved, destination) = if case == 3 {
                    (first, other)
                } else {
                    (axe, victim)
                };
                let response = runner
                    .cast(theft)
                    .target_objects(&[moved, destination])
                    .commit();
                assert_eq!(response.state().stack.len(), 2);
                assert_eq!(response.state().players[1].mana_pool.total(), 0);
                drop(response);
                runner.pass_both_players();
                assert_eq!(runner.state().objects[&moved].controller, P1);
                assert_eq!(
                    runner.state().objects[&moved].attached_to,
                    Some(engine::game::game_object::AttachTarget::Object(destination))
                );
                assert!(runner
                    .state()
                    .stack
                    .iter()
                    .any(|entry| entry.source_id == light));
            } else {
                complete_publication_fund(&mut runner, P1, 1, &[ManaType::Blue]);
                runner.cast(bounce).target_object(victim).resolve();
                assert_eq!(runner.state().objects[&victim].zone, Zone::Hand);
                assert_eq!(runner.state().objects[&victim].damage_marked, 0);
                assert_eq!(runner.state().objects[&light].zone, Zone::Graveyard);
                for object in [first, second, axe, unattached] {
                    assert_eq!(runner.state().objects[&object].zone, Zone::Battlefield);
                }
                assert_continuous_complete(runner.state());
                continue;
            }
        }
        runner.advance_until_stack_empty();
        let WaitingFor::ChooseObjectsSelection {
            player,
            eligible,
            min,
            max,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Light's real resolution-time Equipment choice")
        };
        assert_eq!(*player, P0);
        assert_eq!(*min, 0);
        let expected = match case {
            2 => vec![],
            3 => vec![TargetRef::Object(second)],
            5 | 7 => vec![
                TargetRef::Object(first),
                TargetRef::Object(second),
                TargetRef::Object(axe),
            ],
            _ => vec![TargetRef::Object(first), TargetRef::Object(second)],
        };
        assert_eq!(eligible.len(), expected.len());
        for target in &expected {
            assert!(eligible.contains(target));
        }
        assert_eq!(*max, Some(if expected.is_empty() { 0 } else { 1 }));
        assert_eq!(runner.state().objects[&victim].damage_marked, 6);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
        let frame = runner
            .state()
            .resolution_stack
            .active_ability_continuation()
            .expect("Light's actual parked frame");
        assert_eq!(frame.pending.chain.source_id, light);
        assert!(!eligible.contains(&TargetRef::Object(aura)));
        assert!(!eligible.contains(&TargetRef::Object(unattached)));
        for invalid in [
            vec![TargetRef::Object(aura)],
            vec![TargetRef::Object(unattached)],
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            vec![TargetRef::Object(second), TargetRef::Object(second)],
        ] {
            assert!(runner
                .act(GameAction::SelectTargets { targets: invalid })
                .is_err());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseObjectsSelection { .. }
            ));
            assert_eq!(
                runner
                    .state()
                    .resolution_stack
                    .active_ability_continuation()
                    .unwrap()
                    .pending
                    .chain
                    .source_id,
                light
            );
        }
        let selected = if case == 1 || case == 2 {
            None
        } else if case == 5 {
            Some(axe)
        } else {
            Some(second)
        };
        runner
            .act(GameAction::SelectTargets {
                targets: selected.into_iter().map(TargetRef::Object).collect(),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&second].zone,
            if selected == Some(second) {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&unattached].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&axe].zone,
            Zone::Battlefield,
            "the printed indestructible Equipment survives a lawful selection"
        );
        assert_eq!(
            runner.state().objects[&victim].zone,
            if case == 4 {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&aura].zone,
            if case == 4 {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(runner.state().objects[&light].zone, Zone::Graveyard);
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn u6_native_flashback_fixed_nonmana_and_compound_costs_remain_paid() {
    use engine::game::casting::{can_cast_object_now, current_casting_variant_choice_options};
    use engine::types::game_state::{CastingVariant, StackEntryKind};
    for (name, text, printed, instant, generic, colors, draws) in [
        (
            "Think Twice",
            U22_THINK,
            "{1}{U}",
            true,
            2,
            vec![ManaType::Blue],
            1,
        ),
        (
            "Firebolt",
            U22_FIREBOLT,
            "{R}",
            false,
            4,
            vec![ManaType::Red],
            0,
        ),
        (
            "Battle Screech",
            U22_SCREECH,
            "{2}{W}{W}",
            false,
            0,
            vec![],
            0,
        ),
        (
            "Deep Analysis",
            U22_ANALYSIS,
            "{3}{U}",
            false,
            1,
            vec![ManaType::Blue],
            2,
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Native draw A", "Native draw B", "Native draw C"]);
        let spell = scenario
            .add_spell_to_graveyard(P0, name, instant)
            .from_oracle_text(text)
            .with_mana_cost(complete_publication_cost(printed))
            .id();
        let victim = scenario
            .add_creature(P1, "Firebolt nondefault victim", 3, 2)
            .id();
        let whites = (0..3)
            .map(|_| {
                scenario
                    .add_creature(P0, "White payment creature", 1, 3)
                    .with_color(vec![ManaColor::White])
                    .id()
            })
            .collect::<Vec<_>>();
        let excluded = scenario
            .add_creature(P1, "Foreign white payment exclusion", 1, 3)
            .with_color(vec![ManaColor::White])
            .id();
        let mut runner = scenario.build();
        assert_eq!(
            runner.state().objects[&spell].mana_cost,
            complete_publication_cost(printed)
        );
        if name != "Battle Screech" {
            assert!(
                !can_cast_object_now(runner.state(), P0, spell),
                "a native fixed or compound cost still needs mana"
            );
        }
        complete_publication_fund(&mut runner, P0, generic, &colors);
        let options = current_casting_variant_choice_options(runner.state(), P0, spell);
        assert!(options
            .iter()
            .any(|option| option.variant == CastingVariant::Flashback));
        assert!(can_cast_object_now(runner.state(), P0, spell));
        let hand = runner.state().players[0].hand.len();
        let life = runner.state().players[0].life;
        let cast = runner
            .cast(spell)
            .casting_variant(CastingVariant::Flashback);
        let committed = if name == "Firebolt" {
            cast.target_object(victim).commit()
        } else if name == "Deep Analysis" {
            cast.target_player(P0).commit()
        } else if name == "Battle Screech" {
            cast.pay_cost_with(&whites).commit()
        } else {
            cast.commit()
        };
        assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        assert!(matches!(
            committed.state().stack.last().unwrap().kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Flashback,
                ..
            }
        ));
        assert_eq!(
            committed.state().players[0].life,
            life - if name == "Deep Analysis" { 3 } else { 0 }
        );
        for object in &whites {
            assert_eq!(
                committed.state().objects[object].tapped,
                name == "Battle Screech"
            );
        }
        assert!(!committed.state().objects[&excluded].tapped);
        committed.resolve();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
        assert_eq!(runner.state().players[0].hand.len(), hand + draws);
        if name == "Firebolt" {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        }
        if name == "Battle Screech" {
            let birds = runner
                .state()
                .objects
                .values()
                .filter(|object| {
                    object.zone == Zone::Battlefield && object.is_token && object.name == "Bird"
                })
                .collect::<Vec<_>>();
            assert_eq!(birds.len(), 2);
            for bird in birds {
                assert_eq!((bird.power, bird.toughness), (Some(1), Some(1)));
                assert_eq!(bird.controller, P0);
                assert!(bird.keywords.contains(&Keyword::Flying));
            }
        }
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn u6_full_producers_keep_zone_owner_color_generic_and_target_hostiles() {
    use engine::game::casting::{can_cast_object_now, current_casting_variant_choice_options};
    use engine::game::keywords::effective_flashback_cost;
    use engine::types::game_state::CastPaymentMode;
    for planar in [false, true] {
        for case in 0..4 {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_spell_to_hand_from_oracle(P0, "Return the Past", false, U22_RETURN)
                .as_enchantment()
                .with_mana_cost(complete_publication_cost("{4}{R}{R}"))
                .id();
            let recipient = scenario
                .add_spell_to_graveyard(P0, "Doom Blade", true)
                .from_oracle_text(U22_DOOM)
                .with_mana_cost(complete_publication_cost("{1}{B}"))
                .id();
            let foreign = scenario
                .add_spell_to_graveyard(P1, "Doom Blade", true)
                .from_oracle_text(U22_DOOM)
                .with_mana_cost(complete_publication_cost("{1}{B}"))
                .id();
            let hand = scenario
                .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, U22_DOOM)
                .with_mana_cost(complete_publication_cost("{1}{B}"))
                .id();
            let exile = scenario
                .add_spell_to_exile(P0, "Doom Blade", true)
                .from_oracle_text(U22_DOOM)
                .with_mana_cost(complete_publication_cost("{1}{B}"))
                .id();
            let library = scenario
                .add_spell_to_library_top(P0, "Doom Blade", true)
                .from_oracle_text(U22_DOOM)
                .with_mana_cost(complete_publication_cost("{1}{B}"))
                .id();
            let creature = scenario
                .add_creature_to_graveyard(P0, "Creature card excluded", 2, 2)
                .id();
            let land = scenario
                .add_land_to_graveyard(P0, "Land card excluded")
                .id();
            let victim = scenario
                .add_creature(P1, "Color target discriminator", 3, 4)
                .with_color(vec![if case == 2 {
                    ManaColor::Black
                } else {
                    ManaColor::White
                }])
                .id();
            let mut runner = scenario.build();
            // A deliberately stale off-zone controller is initial fixture data;
            // the grant and casting authority must still use the actual owner.
            runner
                .state_mut()
                .objects
                .get_mut(&foreign)
                .unwrap()
                .controller = P0;
            if planar {
                u22_starting_otaria(&mut runner);
            } else {
                u22_paid_return(&mut runner, source);
            }
            assert!(effective_flashback_cost(runner.state(), recipient).is_some());
            assert_eq!(
                effective_flashback_cost(runner.state(), foreign).is_some(),
                planar
            );
            for object in [hand, exile, library, creature, land] {
                assert!(effective_flashback_cost(runner.state(), object).is_none());
            }
            assert!(!can_cast_object_now(runner.state(), P0, foreign));
            assert!(
                current_casting_variant_choice_options(runner.state(), P0, recipient).is_empty(),
                "unfunded control"
            );
            match case {
                0 => complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]),
                1 => complete_publication_fund(&mut runner, P0, 0, &[ManaType::Black]),
                _ => complete_publication_fund(&mut runner, P0, 1, &[ManaType::Black]),
            }
            let mana = runner.state().players[0].mana_pool.total();
            if case == 3 {
                assert!(can_cast_object_now(runner.state(), P0, recipient));
                runner.cast(recipient).target_object(victim).resolve();
                assert_eq!(runner.state().objects[&recipient].zone, Zone::Exile);
                assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
                assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            } else {
                assert!(!can_cast_object_now(runner.state(), P0, recipient));
                assert!(
                    current_casting_variant_choice_options(runner.state(), P0, recipient)
                        .is_empty()
                );
                let attempt = runner.act(GameAction::CastSpell {
                    object_id: recipient,
                    card_id: runner.state().objects[&recipient].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                });
                if attempt.is_ok() {
                    let WaitingFor::TargetSelection {
                        player,
                        pending_cast,
                        ..
                    } = &runner.state().waiting_for
                    else {
                        panic!("the real uncommitted negative announcement")
                    };
                    assert_eq!(*player, P0);
                    assert_eq!(pending_cast.object_id, recipient);
                    runner.act(GameAction::CancelCast).unwrap();
                }
                assert_eq!(runner.state().players[0].mana_pool.total(), mana);
                assert_eq!(runner.state().objects[&recipient].zone, Zone::Graveyard);
                assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
            }
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn u6_native_nonmana_public_payment_rejects_invalid_choices_and_finishes_the_real_cost() {
    use engine::game::casting::current_casting_variant_choice_options;
    use engine::types::game_state::{CastPaymentMode, CastingVariant};
    for enough in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let screech = scenario
            .add_spell_to_graveyard(P0, "Battle Screech", false)
            .from_oracle_text(U22_SCREECH)
            .with_mana_cost(complete_publication_cost("{2}{W}{W}"))
            .id();
        let whites = (0..if enough { 3 } else { 2 })
            .map(|_| {
                scenario
                    .add_creature(P0, "White tap-cost recipient", 2, 3)
                    .with_color(vec![ManaColor::White])
                    .id()
            })
            .collect::<Vec<_>>();
        let foreign = scenario
            .add_creature(P1, "Foreign white excluded", 2, 3)
            .with_color(vec![ManaColor::White])
            .id();
        let blue = scenario
            .add_creature(P0, "Own blue excluded", 2, 3)
            .with_color(vec![ManaColor::Blue])
            .id();
        let mut runner = scenario.build();
        let options = current_casting_variant_choice_options(runner.state(), P0, screech);
        assert_eq!(
            options
                .iter()
                .any(|option| option.variant == CastingVariant::Flashback),
            enough
        );
        if !enough {
            let attempt = runner.act(GameAction::CastSpell {
                object_id: screech,
                card_id: runner.state().objects[&screech].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            });
            if attempt.is_ok() {
                runner.act(GameAction::CancelCast).unwrap();
            }
            assert_eq!(runner.state().objects[&screech].zone, Zone::Graveyard);
            for object in &whites {
                assert!(!runner.state().objects[object].tapped);
            }
            assert_continuous_complete(runner.state());
            continue;
        }
        runner
            .act(GameAction::CastSpell {
                object_id: screech,
                card_id: runner.state().objects[&screech].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::PayCost {
            player,
            choices,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("the actual three-creature flashback payment")
        };
        assert_eq!(*player, P0);
        assert_eq!(*count, 3);
        assert_eq!(choices.len(), 3);
        for object in &whites {
            assert!(choices.contains(object));
        }
        assert!(!choices.contains(&foreign) && !choices.contains(&blue));
        for invalid in [
            vec![],
            vec![whites[0], whites[1]],
            vec![whites[0], whites[0], whites[1]],
            vec![whites[0], whites[1], foreign],
            vec![whites[0], whites[1], blue],
        ] {
            assert!(runner
                .act(GameAction::SelectCards { cards: invalid })
                .is_err());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::PayCost {
                    player: P0,
                    count: 3,
                    ..
                }
            ));
            for object in &whites {
                assert!(!runner.state().objects[object].tapped);
            }
        }
        runner
            .act(GameAction::SelectCards {
                cards: whites.clone(),
            })
            .unwrap();
        assert_eq!(runner.state().objects[&screech].zone, Zone::Stack);
        for object in &whites {
            assert!(runner.state().objects[object].tapped);
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&screech].zone, Zone::Exile);
        assert_eq!(
            runner
                .state()
                .objects
                .values()
                .filter(|object| object.is_token
                    && object.name == "Bird"
                    && object.zone == Zone::Battlefield)
                .count(),
            2
        );
        assert_continuous_complete(runner.state());
    }
    let mut scenario = GameScenario::new();
    scenario
        .at_phase(Phase::PreCombatMain)
        .with_life(P0, 2)
        .with_library_top(P0, &["Compound draw A", "Compound draw B"]);
    let analysis = scenario
        .add_spell_to_graveyard(P0, "Deep Analysis", false)
        .from_oracle_text(U22_ANALYSIS)
        .with_mana_cost(complete_publication_cost("{3}{U}"))
        .id();
    let mut runner = scenario.build();
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    assert!(
        !current_casting_variant_choice_options(runner.state(), P0, analysis)
            .iter()
            .any(|option| option.variant == CastingVariant::Flashback)
    );
    let attempt = runner.act(GameAction::CastSpell {
        object_id: analysis,
        card_id: runner.state().objects[&analysis].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    if attempt.is_ok() {
        let WaitingFor::TargetSelection {
            player,
            pending_cast,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("compound cost announcement")
        };
        assert_eq!(*player, P0);
        assert_eq!(pending_cast.object_id, analysis);
        let _ = runner.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        });
        assert_ne!(runner.state().objects[&analysis].zone, Zone::Stack);
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner.act(GameAction::CancelCast).unwrap();
        }
    }
    assert_eq!(runner.state().players[0].life, 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 2);
    assert_eq!(runner.state().objects[&analysis].zone, Zone::Graveyard);
    assert!(runner.state().players[0].hand.is_empty());
    assert_continuous_complete(runner.state());
}

#[test]
fn u6_return_live_graveyard_entries_bounce_reentry_controller_and_multiple_sources() {
    use engine::game::casting::{can_cast_object_now, current_casting_variant_choice_options};
    use engine::game::keywords::effective_flashback_cost;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(
            player,
            &[
                "Live draw A",
                "Live draw B",
                "Live draw C",
                "Live draw D",
                "Live draw E",
            ],
        );
    }
    let source = scenario
        .add_spell_to_hand_from_oracle(P0, "Return the Past", false, U22_RETURN)
        .as_enchantment()
        .with_mana_cost(complete_publication_cost("{4}{R}{R}"))
        .id();
    let own = scenario
        .add_spell_to_graveyard(P0, "Doom Blade", true)
        .from_oracle_text(U22_DOOM)
        .with_mana_cost(complete_publication_cost("{1}{B}"))
        .id();
    let foreign = scenario
        .add_spell_to_graveyard(P1, "Doom Blade", true)
        .from_oracle_text(U22_DOOM)
        .with_mana_cost(complete_publication_cost("{1}{B}"))
        .id();
    let later = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, U22_DIVINATION)
        .with_mana_cost(complete_publication_cost("{2}{U}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Disperse", true, U22_DISPERSE)
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    let donate = scenario
        .add_spell_to_hand_from_oracle(P0, "Donate", false, U22_DONATE)
        .with_mana_cost(complete_publication_cost("{2}{U}"))
        .id();
    let lier = scenario
        .add_creature_to_hand_from_oracle(P0, "Lier, Disciple of the Drowned", 3, 4, U22_LIER)
        .with_mana_cost(complete_publication_cost("{3}{U}{U}"))
        .id();
    let bounce_again = scenario
        .add_spell_to_hand_from_oracle(P0, "Disperse", true, U22_DISPERSE)
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    let victim = scenario
        .add_creature(P1, "Legal nonblack hostile target", 4, 4)
        .with_color(vec![ManaColor::White])
        .id();
    let mut runner = scenario.build();
    assert!(effective_flashback_cost(runner.state(), own).is_none());
    u22_paid_return(&mut runner, source);
    assert!(effective_flashback_cost(runner.state(), own).is_some());
    assert!(effective_flashback_cost(runner.state(), foreign).is_none());
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    runner.cast(later).resolve();
    assert_eq!(runner.state().objects[&later].zone, Zone::Graveyard);
    assert!(
        effective_flashback_cost(runner.state(), later).is_some(),
        "a card entering after the source receives the live grant"
    );
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    let incarnation = runner.state().objects[&source].incarnation;
    runner.cast(bounce).target_object(source).resolve();
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
    assert_ne!(runner.state().objects[&source].incarnation, incarnation);
    for object in [own, later] {
        assert!(effective_flashback_cost(runner.state(), object).is_none());
        assert!(current_casting_variant_choice_options(runner.state(), P0, object).is_empty());
    }
    u22_paid_return(&mut runner, source);
    assert!(effective_flashback_cost(runner.state(), own).is_some());
    complete_publication_fund(&mut runner, P0, 3, &[ManaType::Blue, ManaType::Blue]);
    runner.cast(lier).resolve();
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    runner
        .cast(donate)
        .target_player(P1)
        .target_object(source)
        .resolve();
    assert_eq!(runner.state().objects[&source].owner, P0);
    assert_eq!(runner.state().objects[&source].controller, P1);
    assert!(
        effective_flashback_cost(runner.state(), own).is_some(),
        "the independent Lier authority remains"
    );
    assert!(
        effective_flashback_cost(runner.state(), foreign).is_none(),
        "Return's new controller is not the active player yet"
    );
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    let hand_before_lier_cast = runner.state().players[0].hand.len();
    runner.cast(later).resolve();
    assert_eq!(runner.state().objects[&later].zone, Zone::Exile);
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before_lier_cast + 2
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(bounce_again).target_object(lier).resolve();
    assert!(
        effective_flashback_cost(runner.state(), own).is_none(),
        "removing the last applicable authority ends the grant"
    );
    assert_continuous_complete(runner.state());
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert!(effective_flashback_cost(runner.state(), foreign).is_some());
    assert!(effective_flashback_cost(runner.state(), own).is_none());
    complete_publication_fund(&mut runner, P1, 1, &[ManaType::Black]);
    assert!(can_cast_object_now(runner.state(), P1, foreign));
    runner.cast(foreign).target_object(victim).resolve();
    assert_eq!(runner.state().objects[&foreign].zone, Zone::Exile);
    assert_continuous_complete(runner.state());
}

fn u22_roll_planar_face(runner: &mut GameRunner, face: engine::game::planechase::PlanarDieFace) {
    let seed = (0..512_u64).find(|seed| {
        let mut probe = GameRunner::from_state(runner.state().clone());
        probe.state_mut().rng = engine::types::game_state::GameRng::seed_from_u64(*seed);
        probe.act(GameAction::RollPlanarDie).is_ok_and(|result| result.events.iter().any(|event|
            matches!(event, engine::types::events::GameEvent::PlanarDieRolled { face: rolled, .. } if *rolled == face)))
    }).expect("a deterministic seed for the public planar die action");
    runner.state_mut().rng = engine::types::game_state::GameRng::seed_from_u64(seed);
    let result = runner.act(GameAction::RollPlanarDie).unwrap();
    assert!(result.events.iter().any(|event| matches!(event, engine::types::events::GameEvent::PlanarDieRolled { face: rolled, .. } if *rolled == face)));
}

#[test]
fn u6_otaria_lawful_plane_grants_both_players_both_types_and_keeps_instant_timing() {
    use engine::game::casting::can_cast_object_now;
    use engine::types::game_state::CastingVariant;
    for caster in [P0, P1] {
        for sorcery in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            for player in [P0, P1] {
                scenario.with_library_top(
                    player,
                    &[
                        "Planar draw A",
                        "Planar draw B",
                        "Planar draw C",
                        "Planar draw D",
                    ],
                );
            }
            let recipient = scenario
                .add_spell_to_graveyard(
                    caster,
                    if sorcery { "Divination" } else { "Doom Blade" },
                    !sorcery,
                )
                .from_oracle_text(if sorcery { U22_DIVINATION } else { U22_DOOM })
                .with_mana_cost(complete_publication_cost(if sorcery {
                    "{2}{U}"
                } else {
                    "{1}{B}"
                }))
                .id();
            let victim = scenario
                .add_creature(
                    if caster == P0 { P1 } else { P0 },
                    "Planar hostile victim",
                    4,
                    4,
                )
                .with_color(vec![ManaColor::White])
                .id();
            let creature = scenario
                .add_creature_to_graveyard(caster, "Nonspell excluded from Otaria", 2, 2)
                .id();
            let mut runner = scenario.build();
            let (otaria, _) = u22_starting_otaria(&mut runner);
            if caster == P1 && sorcery {
                complete_publication_fund(&mut runner, P1, 2, &[ManaType::Blue]);
                runner.act(GameAction::PassPriority).unwrap();
                assert!(
                    !can_cast_object_now(runner.state(), P1, recipient),
                    "universal Flashback preserves sorcery timing on the other player's turn"
                );
                runner.act(GameAction::PassPriority).unwrap();
                runner.advance_to_combat();
                runner.declare_attackers(&[]).unwrap();
                runner.advance_to_end_step();
                runner.pass_both_players();
                runner.advance_to_phase(Phase::PreCombatMain);
                assert_eq!(runner.state().active_player, P1);
            } else if caster == P1 {
                runner.act(GameAction::PassPriority).unwrap();
                assert_eq!(
                    runner.state().active_player,
                    P0,
                    "the other-turn instant reach guard"
                );
            }
            complete_publication_fund(
                &mut runner,
                caster,
                if sorcery { 2 } else { 1 },
                &[if sorcery {
                    ManaType::Blue
                } else {
                    ManaType::Black
                }],
            );
            assert_eq!(runner.state().objects[&otaria].zone, Zone::Command);
            assert!(can_cast_object_now(runner.state(), caster, recipient));
            assert!(!can_cast_object_now(runner.state(), caster, creature));
            assert!(!can_cast_object_now(
                runner.state(),
                if caster == P0 { P1 } else { P0 },
                recipient
            ));
            let hand = runner.state().players[caster.0 as usize].hand.len();
            let cast = runner
                .cast(recipient)
                .casting_variant(CastingVariant::Flashback);
            let committed = if sorcery {
                cast.commit()
            } else {
                cast.target_object(victim).commit()
            };
            assert_eq!(committed.state().objects[&recipient].zone, Zone::Stack);
            assert_eq!(
                committed.state().players[caster.0 as usize]
                    .mana_pool
                    .total(),
                0
            );
            committed.resolve();
            assert_eq!(runner.state().objects[&recipient].zone, Zone::Exile);
            assert_eq!(
                runner.state().players[caster.0 as usize].hand.len(),
                hand + if sorcery { 2 } else { 0 }
            );
            assert_eq!(
                runner.state().objects[&victim].zone,
                if sorcery {
                    Zone::Battlefield
                } else {
                    Zone::Graveyard
                }
            );
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn u6_otaria_public_planeswalk_away_back_and_chaos_preserve_command_membership() {
    use engine::game::keywords::effective_flashback_cost;
    use engine::game::planechase::{active_plane, PlanarDieFace};
    for second_source in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for player in [P0, P1] {
            scenario.with_library_top(
                player,
                &["Turn draw A", "Turn draw B", "Turn draw C", "Turn draw D"],
            );
        }
        let own = scenario
            .add_spell_to_graveyard(P0, "Divination", false)
            .from_oracle_text(U22_DIVINATION)
            .with_mana_cost(complete_publication_cost("{2}{U}"))
            .id();
        let foreign = scenario
            .add_spell_to_graveyard(P1, "Doom Blade", true)
            .from_oracle_text(U22_DOOM)
            .with_mana_cost(complete_publication_cost("{1}{B}"))
            .id();
        let command_return = scenario
            .add_spell_to_hand_from_oracle(P0, "Return the Past", false, U22_RETURN)
            .as_enchantment()
            .with_mana_cost(complete_publication_cost("{4}{R}{R}"))
            .id();
        let lier = scenario
            .add_creature_to_hand_from_oracle(P0, "Lier, Disciple of the Drowned", 3, 4, U22_LIER)
            .with_mana_cost(complete_publication_cost("{3}{U}{U}"))
            .id();
        let mut runner = scenario.build();
        // Initial Command placement is the inactive-zone control, before any producer.
        runner.state_mut().players[0]
            .hand
            .retain(|id| *id != command_return);
        runner
            .state_mut()
            .objects
            .get_mut(&command_return)
            .unwrap()
            .zone = Zone::Command;
        runner.state_mut().command_zone.push_back(command_return);
        assert_eq!(
            runner.state().objects[&command_return]
                .static_definitions
                .iter_unchecked()
                .count(),
            1
        );
        assert!(runner.state().objects[&command_return]
            .static_definitions
            .iter_unchecked()
            .all(|entry| entry.active_zones.is_empty()));
        assert!(effective_flashback_cost(runner.state(), own).is_none());
        let (otaria, tazeem) = u22_starting_otaria(&mut runner);
        if second_source {
            complete_publication_fund(&mut runner, P0, 3, &[ManaType::Blue, ManaType::Blue]);
            runner.cast(lier).resolve();
            assert_eq!(runner.state().objects[&lier].zone, Zone::Battlefield);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_continuous_complete(runner.state());
        }
        for object in [own, foreign] {
            assert!(effective_flashback_cost(runner.state(), object).is_some());
        }
        u22_roll_planar_face(&mut runner, PlanarDieFace::Planeswalk);
        assert!(runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == otaria));
        runner.advance_until_stack_empty();
        assert_eq!(active_plane(runner.state()), Some(tazeem));
        assert_eq!(runner.state().objects[&otaria].zone, Zone::Command);
        assert!(runner.state().objects[&otaria].face_down);
        assert!(runner.state().planar_deck.contains(&otaria));
        assert!(!runner.state().command_zone.contains(&otaria));
        assert_eq!(
            effective_flashback_cost(runner.state(), own).is_some(),
            second_source,
            "only the independent paid Lier can retain own permission while Otaria is face down"
        );
        assert!(
            effective_flashback_cost(runner.state(), foreign).is_none(),
            "a face-down plane in the planar deck has no universal grant"
        );
        assert_continuous_complete(runner.state());
        complete_publication_fund(&mut runner, P0, 1, &[]);
        u22_roll_planar_face(&mut runner, PlanarDieFace::Planeswalk);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner.advance_until_stack_empty();
        assert_eq!(active_plane(runner.state()), Some(otaria));
        assert!(runner.state().command_zone.contains(&otaria));
        assert!(!runner.state().planar_deck.contains(&otaria));
        assert!(!runner.state().objects[&otaria].face_down);
        for object in [own, foreign] {
            assert!(effective_flashback_cost(runner.state(), object).is_some());
        }
        complete_publication_fund(&mut runner, P0, 2, &[]);
        u22_roll_planar_face(&mut runner, PlanarDieFace::Chaos);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert!(runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == otaria));
        assert!(runner.state().extra_turns.is_empty());
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().extra_turns.len(), 1);
        assert_eq!(runner.state().extra_turns[0].player, P0);
        assert_eq!(runner.state().extra_turns[0].anchor, P0);
        assert_continuous_complete(runner.state());
        let turn = runner.state().turn_number;
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(
            runner.state().active_player,
            P0,
            "the printed Chaos ability produced the extra turn"
        );
        assert!(runner.state().turn_number > turn);
        assert!(runner.state().extra_turns.is_empty());
        assert_eq!(active_plane(runner.state()), Some(otaria));
        assert_eq!(runner.state().planar_controller, Some(P0));
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn u6_return_empty_graveyard_later_entry_and_turn_timing_are_live() {
    use engine::game::casting::can_cast_object_now;
    use engine::game::keywords::effective_flashback_cost;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(
            player,
            &[
                "Timing draw A",
                "Timing draw B",
                "Timing draw C",
                "Timing draw D",
                "Timing draw E",
                "Timing draw F",
                "Timing draw G",
                "Timing draw H",
                "Timing draw I",
                "Timing draw J",
                "Timing draw K",
                "Timing draw L",
            ],
        );
    }
    let source = scenario
        .add_spell_to_hand_from_oracle(P0, "Return the Past", false, U22_RETURN)
        .as_enchantment()
        .with_mana_cost(complete_publication_cost("{4}{R}{R}"))
        .id();
    let later = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, U22_DIVINATION)
        .with_mana_cost(complete_publication_cost("{2}{U}"))
        .id();
    let retained = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, U22_DIVINATION)
        .with_mana_cost(complete_publication_cost("{2}{U}"))
        .id();
    let instant = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, U22_DOOM)
        .with_mana_cost(complete_publication_cost("{1}{B}"))
        .id();
    let victim = scenario
        .add_creature(P1, "First legal timing victim", 3, 4)
        .with_color(vec![ManaColor::White])
        .id();
    let another = scenario
        .add_creature(P1, "Second legal timing victim", 3, 4)
        .with_color(vec![ManaColor::Green])
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().players[0].graveyard.is_empty());
    u22_paid_return(&mut runner, source);
    assert!(runner.state().players[0].graveyard.is_empty());
    assert!(effective_flashback_cost(runner.state(), later).is_none());
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    runner.cast(later).resolve();
    assert_eq!(runner.state().objects[&later].zone, Zone::Graveyard);
    assert!(effective_flashback_cost(runner.state(), later).is_some());
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    runner.cast(retained).resolve();
    assert_eq!(runner.state().objects[&retained].zone, Zone::Graveyard);
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Black]);
    runner.cast(instant).target_object(victim).resolve();
    assert_eq!(runner.state().objects[&instant].zone, Zone::Graveyard);
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    let parked = runner.cast(later).commit();
    assert_eq!(parked.state().stack.len(), 1);
    drop(parked);
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    assert!(
        !can_cast_object_now(runner.state(), P0, retained),
        "a fully funded sorcery cannot be cast above a parked spell"
    );
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Black]);
    assert!(
        can_cast_object_now(runner.state(), P0, instant),
        "an instant remains legal above a parked spell"
    );
    runner.cast(instant).target_object(another).resolve();
    assert_eq!(runner.state().objects[&instant].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&later].zone, Zone::Exile);
    assert_continuous_complete(runner.state());
    assert!(
        engine::game::keywords::effective_flashback_cost(runner.state(), instant).is_none(),
        "exile is outside the live population"
    );
    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    assert!(effective_flashback_cost(runner.state(), retained).is_some());
    assert!(
        !can_cast_object_now(runner.state(), P0, retained),
        "a funded sorcery still needs main-phase timing"
    );
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert!(
        effective_flashback_cost(runner.state(), retained).is_none(),
        "Return turns off during the other player's turn"
    );
    assert_continuous_complete(runner.state());
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    assert!(effective_flashback_cost(runner.state(), retained).is_some());
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    assert!(can_cast_object_now(runner.state(), P0, retained));
    runner.cast(retained).resolve();
    assert_eq!(runner.state().objects[&retained].zone, Zone::Exile);
    assert_continuous_complete(runner.state());
}

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

#[test]
fn flowstone_surge_applies_mixed_pt_to_your_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: an enchantment carrying the mixed anthem (real parse + synthesis
    // pipeline, then flipped to an enchantment permanent).
    let _surge = scenario
        .add_creature_from_oracle(P0, "Flowstone Surge", 0, 0, FLOWSTONE_SURGE)
        .as_enchantment()
        .id();

    // A creature you control — gets +1/-1.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // An opponent's creature — excluded by "you control".
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    // CR 613.4c: a creature you control gets +1/-1 → 3/1.
    assert_eq!(
        effective_pt(&mut runner, ally),
        (3, 1),
        "a creature you control gets +1/-1: 2/2 → 3/1"
    );

    // CR 109.4: "you control" excludes the opponent's creature.
    assert_eq!(
        effective_pt(&mut runner, foe),
        (2, 2),
        "an opponent's creature must NOT be affected ('you control')"
    );
}

#[test]
fn flowstone_surge_effect_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let surge = scenario
        .add_creature_from_oracle(P0, "Flowstone Surge", 0, 0, FLOWSTONE_SURGE)
        .as_enchantment()
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, ally),
        (3, 1),
        "baseline: ally is 3/1 while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != surge);
        state.objects.remove(&surge);
    }
    assert_eq!(
        effective_pt(&mut runner, ally),
        (2, 2),
        "ally reverts to base 2/2 once the source is gone"
    );
}

const ALMOST: &str = "Enchant creature\nEnchanted creature has base power and toughness 9/10 and has indestructible.";

const MACE: &str = "Equipped creature gets +1/+1 for each charge counter on this Equipment and has vigilance. (Attacking doesn't cause it to tap.)\nWhenever a creature you control enters, put a charge counter on this Equipment.\nEquip {3}";

const SCYTHE: &str = "Job select\nAt the beginning of your end step, put a soul counter on this Equipment for each player who lost life this turn.\nEquipped creature gets +1/+1 for each soul counter on this Equipment and is an Assassin in addition to its other types.\nDeath Sickle — Equip {2}";

const RITEKNIFE: &str = "Equipped creature gets +1/+0 for each blood counter on this Equipment and has \"{T}, Sacrifice a creature: Put a blood counter on Rakdos Riteknife.\"\n{B}{R}, Sacrifice this Equipment: Target player sacrifices a permanent of their choice for each blood counter on this Equipment.\nEquip {2}";

const JODAH: &str = "Legendary creatures you control get +X/+X, where X is the number of legendary creatures you control.\nWhenever you cast a legendary spell from your hand, exile cards from the top of your library until you exile a legendary nonland card with lesser mana value. You may cast that card without paying its mana cost. Put the rest on the bottom of your library in a random order.";

const HANCOCK: &str = "Each other creature you control that's a Zombie or Mutant gets +X/+X, where X is the number of counters on Hancock.\nUndying (When this creature dies, if it had no +1/+1 counters on it, return it to the battlefield under its owner's control with a +1/+1 counter on it.)";

const ZINNIA: &str = "Flying\nZinnia gets +X/+0, where X is the number of other creatures you control with base power 1.\nCreature spells you cast gain offspring {2} as you cast them. (You may pay an additional {2} as you cast a creature spell. If you do, when that creature enters, create a 1/1 token copy of it.)";

const DROP_BEAR: &str = "Flash, haste\nDrop Bear has power and toughness each equal to the number of Forests you control plus the number of Bears you control.\nWhen this creature enters, it fights up to one target creature.";

const ELTUREL: &str = "Trample, myriad (Whenever this creature attacks, for each opponent other than defending player, you may create a token copy that's tapped and attacking that player or a planeswalker they control. Exile the tokens at end of combat.)\nAs long as this creature is attacking, it gets +X/+0, where X is the number of lands defending player controls.";

const OYAMINARTOK: &str = "Oyaminartok, Polar Werebear has hexproof if it hasn't dealt damage yet.\nWhenever Oyaminartok deals combat damage to a player, create a Food token.\n{U}, Sacrifice a Food: Draft a card from Oyaminartok's spellbook. When you do, add {U}{U}{U}. Spend this mana only to cast blue creature spells.";

const AURELIA: &str = "Flying\nMentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\nAt the beginning of combat on your turn, choose up to one target creature you control. Until end of turn, that creature gets +2/+0, gains trample if it's red, and gains vigilance if it's white.";

const ENTITY: &str = "Changeling (This card is every creature type.)\n{X}: Until end of turn, creatures you control have base power and toughness X/X and gain all creature types.";

const SAFEWRIGHT: &str = "Flash\nConvoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's color.)\nWhen this creature enters, choose a creature type. Other permanents you control of that type gain hexproof and indestructible until end of turn.";

const ARCHON: &str = "Flying\nOther creatures you control that are enchanted by Auras you control have base power and toughness 4/4 and have flying.";
const LENS: &str = "{T}: Target permanent becomes the color of your choice until end of turn.";
const GREAVES: &str = "Equipped creature has haste and shroud. (It can't be the target of spells or abilities.)\nEquip {0}";
const BLOOM: &str = "Exile target creature card from a graveyard, then create X 1/1 green Saproling creature tokens, where X is the exiled card's toughness.";
const VAROLZ: &str = "Each creature card in your graveyard has scavenge. The scavenge cost is equal to its mana cost. (Exile a creature card from your graveyard and pay its mana cost: Put a number of +1/+1 counters equal to that card's power on target creature. Scavenge only as a sorcery.)\nSacrifice another creature: Regenerate Varolz.";

#[test]
fn complete_continuous_archon_attachment_qualifiers_remain_live() {
    const PACIFISM: &str = "Enchant creature\nEnchanted creature can't attack or block.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archon = scenario
        .add_creature_to_hand_from_oracle(P0, "Archon of the Wild Rose", 4, 4, ARCHON)
        .with_subtypes(vec!["Archon"])
        .with_mana_cost(complete_publication_cost("{2}{W}{W}"))
        .id();
    let own = scenario.add_creature(P0, "Own Aura recipient", 2, 7).id();
    let foreign_aura_host = scenario
        .add_creature(P0, "Foreign Aura recipient", 3, 6)
        .id();
    let unenchanted = scenario.add_creature(P0, "Unenchanted creature", 6, 7).id();
    let foreign = scenario
        .add_creature(P1, "Foreign creature with own Aura", 5, 8)
        .id();
    let aura = scenario
        .add_spell_to_hand_from_oracle(P0, "Pacifism", false, PACIFISM)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(complete_publication_cost("{1}{W}"))
        .id();
    let foreign_aura = scenario
        .add_enchantment_from_oracle(P1, "Pacifism", PACIFISM)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(complete_publication_cost("{1}{W}"))
        .id();
    let other_own_aura = scenario
        .add_enchantment_from_oracle(P0, "Pacifism", PACIFISM)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(complete_publication_cost("{1}{W}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    let mut runner = scenario.build();
    // Ordinary pre-existing attachment fixtures provide independent controller negatives.
    engine::game::effects::attach::attach_to(runner.state_mut(), foreign_aura, foreign_aura_host);
    engine::game::effects::attach::attach_to(runner.state_mut(), other_own_aura, foreign);
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::White]);
    runner.cast(aura).target_object(own).resolve();
    assert_eq!(effective_pt(&mut runner, own), (2, 7));
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::White, ManaType::White]);
    runner.cast(archon).resolve();
    assert_eq!(effective_pt(&mut runner, own), (4, 4));
    assert!(runner.state().objects[&own]
        .keywords
        .contains(&Keyword::Flying));
    for (object, expected) in [
        (foreign_aura_host, (3, 6)),
        (unenchanted, (6, 7)),
        (foreign, (5, 8)),
    ] {
        assert_eq!(effective_pt(&mut runner, object), expected);
        assert!(!runner.state().objects[&object]
            .keywords
            .contains(&Keyword::Flying));
    }
    assert_eq!(effective_pt(&mut runner, archon), (4, 4));
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(aura).resolve();
    assert_eq!(effective_pt(&mut runner, own), (2, 7));
    assert!(!runner.state().objects[&own]
        .keywords
        .contains(&Keyword::Flying));
    assert!(runner.state().objects[&archon]
        .keywords
        .contains(&Keyword::Flying));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_blade_real_reconfigure_preserves_existing_color_and_types() {
    use engine::types::ability::Effect;
    for detach in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let blade = scenario.add_creature_from_oracle(P0, "Blade of the Oni", 3, 1,
            "Menace\nEquipped creature has base power and toughness 5/5, has menace, and is a black Demon in addition to its other colors and types.\nReconfigure {2}{B}{B} ({2}{B}{B}: Attach to target creature you control; or unattach from a creature. Reconfigure only as a sorcery. While attached, this isn't a creature.)")
            .as_artifact().as_creature().with_subtypes(vec!["Equipment", "Demon"]).with_mana_cost(complete_publication_cost("{1}{B}")).id();
        let host = scenario
            .add_creature(P0, "Red Goblin host", 2, 7)
            .with_subtypes(vec!["Goblin"])
            .with_color(vec![ManaColor::Red])
            .id();
        let other = scenario.add_creature(P0, "Other host", 4, 6).id();
        let foreign = scenario.add_creature(P1, "Foreign host", 6, 8).id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Disperse",
                true,
                "Return target nonland permanent to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{1}{U}"))
            .id();
        let mut runner = scenario.build();
        let attach = engine::game::casting::activated_ability_definitions(runner.state(), blade)
            .into_iter()
            .find(|(_, definition)| matches!(definition.effect.as_ref(), Effect::Attach { .. }))
            .unwrap()
            .0;
        assert!(runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: blade,
                ability_index: attach
            })
            .is_err());
        complete_publication_fund(&mut runner, P0, 2, &[ManaType::Black, ManaType::Black]);
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: blade,
                ability_index: attach,
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real Reconfigure target");
        };
        assert_eq!(*player, P0);
        assert_eq!(target_slots.len(), 1);
        assert!(!target_slots[selection.current_slot].optional);
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(host)));
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(other)));
        assert!(!target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(blade)));
        assert!(!target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(foreign)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(host)),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(effective_pt(&mut runner, host), (5, 5));
        assert!(runner.state().objects[&host]
            .keywords
            .contains(&Keyword::Menace));
        assert_eq!(
            runner.state().objects[&host].color,
            vec![ManaColor::Red, ManaColor::Black]
        );
        assert!(runner.state().objects[&host]
            .card_types
            .subtypes
            .iter()
            .any(|t| t == "Goblin"));
        assert!(runner.state().objects[&host]
            .card_types
            .subtypes
            .iter()
            .any(|t| t == "Demon"));
        assert!(!runner.state().objects[&blade]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature));
        assert_eq!(effective_pt(&mut runner, foreign), (6, 8));
        assert_continuous_complete(runner.state());
        if detach {
            let index = engine::game::casting::activated_ability_definitions(runner.state(), blade)
                .into_iter()
                .find(|(_, definition)| {
                    matches!(definition.effect.as_ref(), Effect::UnattachAll { .. })
                })
                .unwrap()
                .0;
            complete_publication_fund(&mut runner, P0, 2, &[ManaType::Black, ManaType::Black]);
            runner.activate(blade, index).resolve();
            assert_eq!(effective_pt(&mut runner, blade), (3, 1));
            assert!(runner.state().objects[&blade]
                .keywords
                .contains(&Keyword::Menace));
        } else {
            complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
            runner.cast(bounce).target_object(blade).resolve();
            assert_eq!(runner.state().objects[&blade].zone, Zone::Hand);
        }
        assert_eq!(effective_pt(&mut runner, host), (2, 7));
        assert!(!runner.state().objects[&host]
            .keywords
            .contains(&Keyword::Menace));
        assert_eq!(runner.state().objects[&host].color, vec![ManaColor::Red]);
        assert_eq!(
            runner.state().objects[&host].card_types.subtypes,
            vec!["Goblin".to_string()]
        );
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_alpine_named_opponent_land_has_real_mana_grant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let named = scenario
        .add_land_from_oracle(P1, "Taiga", "({T}: Add {R} or {G}.)")
        .with_subtypes(vec!["Mountain", "Forest"])
        .id();
    let own = scenario
        .add_land_from_oracle(P0, "Taiga", "({T}: Add {R} or {G}.)")
        .with_subtypes(vec!["Mountain", "Forest"])
        .id();
    let unmatched = scenario
        .add_land_from_oracle(P1, "Tropical Island", "({T}: Add {G} or {U}.)")
        .with_subtypes(vec!["Forest", "Island"])
        .id();
    let moon = scenario.add_spell_to_hand_from_oracle(P0, "Alpine Moon", false,
        "As this enchantment enters, choose a nonbasic land card name.\nLands your opponents control with the chosen name lose all land types and abilities, and they gain \"{T}: Add one mana of any color.\"")
        .as_enchantment().with_mana_cost(complete_publication_cost("{R}")).id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    let mut runner = scenario.build();
    complete_publication_fund(&mut runner, P0, 0, &[ManaType::Red]);
    runner.cast(moon).commit();
    runner.resolve_top();
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        source,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("real nonbasic name choice");
    };
    assert_eq!(*player, P0);
    assert_eq!(
        source.as_ref().unwrap().prompt.identity.reference.object_id,
        moon
    );
    assert!(matches!(
        choice_type,
        engine::types::ability::ChoiceType::CardName
    ));
    assert!(
        options.is_empty(),
        "CardName uses the public free-entry domain"
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "Taiga".into(),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&named]
        .card_types
        .subtypes
        .is_empty());
    assert_eq!(
        runner.state().objects[&own].card_types.subtypes,
        vec!["Mountain".to_string(), "Forest".to_string()]
    );
    assert_eq!(
        runner.state().objects[&unmatched].card_types.subtypes,
        vec!["Forest".to_string(), "Island".to_string()]
    );
    assert_eq!(runner.state().objects[&named].abilities.len(), 1);
    assert_continuous_complete(runner.state());
    runner.act(GameAction::PassPriority).unwrap();
    runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: named,
            ability_index: 0,
        })
        .unwrap();
    let WaitingFor::ChooseManaColor {
        player,
        choice: engine::types::game_state::ManaChoicePrompt::SingleColor { options },
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual granted mana color choice");
    };
    assert_eq!(*player, P1);
    assert_eq!(
        options,
        &vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green
        ]
    );
    runner
        .act(GameAction::ChooseManaColor {
            choice: engine::types::game_state::ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&named].tapped);
    assert_eq!(runner.state().players[1].mana_pool.total(), 1);
    assert_eq!(
        runner.state().players[1]
            .mana_pool
            .count_color(ManaType::Blue),
        1
    );
    assert_continuous_complete(runner.state());
    runner.act(GameAction::PassPriority).unwrap();
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(moon).resolve();
    assert_eq!(
        runner.state().objects[&named].card_types.subtypes,
        vec!["Mountain".to_string(), "Forest".to_string()]
    );
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_phantasmal_optional_targets_and_all_illegal_resolution() {
    use engine::types::game_state::CastPaymentMode;
    for (choose, all_illegal) in [(false, false), (true, false), (true, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario
            .add_creature(P0, "First red Goblin", 2, 7)
            .with_subtypes(vec!["Goblin"])
            .with_color(vec![ManaColor::Red])
            .id();
        let second = scenario
            .add_creature(P1, "Second green Bear", 6, 8)
            .with_subtypes(vec!["Bear"])
            .with_color(vec![ManaColor::Green])
            .id();
        let untouched = scenario.add_creature(P0, "Untouched creature", 5, 6).id();
        let spell = scenario.add_spell_to_hand_from_oracle(P0, "Phantasmal Form", true,
            "Until end of turn, up to two target creatures each have base power and toughness 3/3, gain flying, and become blue Illusions in addition to their other colors and types.\nDraw a card.")
            .with_mana_cost(complete_publication_cost("{2}{U}")).id();
        let bounce = [0, 1].map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Unsummon",
                    true,
                    "Return target creature to its owner's hand.",
                )
                .with_mana_cost(complete_publication_cost("{U}"))
                .id()
        });
        let draw = scenario.add_card_to_library_top(P0, "Specific draw witness");
        let mut runner = scenario.build();
        complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id: runner.state().objects[&spell].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::default(),
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Phantasmal optional target announcement");
        };
        assert_eq!(*player, P0);
        assert!(target_slots[selection.current_slot].optional);
        for id in [first, second, untouched] {
            assert!(target_slots[selection.current_slot]
                .legal_targets
                .contains(&TargetRef::Object(id)));
        }
        if choose {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(first)),
                })
                .unwrap();
            let WaitingFor::TargetSelection {
                player,
                target_slots,
                selection,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("second nondefault target");
            };
            assert_eq!(*player, P0);
            assert!(target_slots[selection.current_slot].optional);
            assert!(target_slots[selection.current_slot]
                .legal_targets
                .contains(&TargetRef::Object(second)));
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(second)),
                })
                .unwrap();
        } else {
            runner
                .act(GameAction::ChooseTarget { target: None })
                .unwrap();
        }
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(
            runner.state().stack[0].ability().unwrap().targets.len(),
            if choose { 2 } else { 0 }
        );
        if all_illegal {
            complete_publication_fund(&mut runner, P0, 0, &[ManaType::Blue, ManaType::Blue]);
            runner.cast(bounce[0]).target_object(first).commit();
            runner.cast(bounce[1]).target_object(second).commit();
            assert_eq!(
                runner.state().stack.len(),
                3,
                "both public removals are above the original spell"
            );
        }
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&draw].zone,
            if all_illegal {
                Zone::Library
            } else {
                Zone::Hand
            }
        );
        assert_eq!(effective_pt(&mut runner, untouched), (5, 6));
        if all_illegal {
            for id in [first, second] {
                assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
            }
        } else if choose {
            for id in [first, second] {
                assert_eq!(effective_pt(&mut runner, id), (3, 3));
                assert!(runner.state().objects[&id]
                    .keywords
                    .contains(&Keyword::Flying));
                assert!(runner.state().objects[&id].color.contains(&ManaColor::Blue));
                assert!(runner.state().objects[&id]
                    .card_types
                    .subtypes
                    .iter()
                    .any(|t| t == "Illusion"));
            }
            assert!(runner.state().objects[&first]
                .color
                .contains(&ManaColor::Red));
            assert!(runner.state().objects[&first]
                .card_types
                .subtypes
                .iter()
                .any(|t| t == "Goblin"));
            assert!(runner.state().objects[&second]
                .color
                .contains(&ManaColor::Green));
            assert!(runner.state().objects[&second]
                .card_types
                .subtypes
                .iter()
                .any(|t| t == "Bear"));
        } else {
            assert_eq!(effective_pt(&mut runner, first), (2, 7));
            assert_eq!(effective_pt(&mut runner, second), (6, 8));
            for id in [first, second] {
                assert!(!runner.state().objects[&id]
                    .keywords
                    .contains(&Keyword::Flying));
            }
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_archetypes_deny_later_hosted_grants_and_resume_after_departure() {
    for (name, power, toughness, cost, text, keyword, grant_name, grant_cost, grant_text) in [
        ("Archetype of Courage", 2, 2, "{1}{W}{W}", "Creatures you control have first strike.\nCreatures your opponents control lose first strike and can't have or gain first strike.", Keyword::FirstStrike,
            "Knighthood", "{2}{W}", "Creatures you control have first strike."),
        ("Archetype of Imagination", 3, 2, "{4}{U}{U}", "Creatures you control have flying.\nCreatures your opponents control lose flying and can't have or gain flying.", Keyword::Flying,
            "Levitation", "{2}{U}{U}", "Creatures you control have flying."),
        ("Archetype of Finality", 2, 3, "{4}{B}{B}", "Creatures you control have deathtouch.\nCreatures your opponents control lose deathtouch and can't have or gain deathtouch.", Keyword::Deathtouch,
            "Ogre Slumlord", "{3}{B}{B}", "Whenever another nontoken creature dies, you may create a 1/1 black Rat creature token.\nRats you control have deathtouch."),
        ("Archetype of Aggression", 3, 2, "{1}{R}{R}", "Creatures you control have trample.\nCreatures your opponents control lose trample and can't have or gain trample.", Keyword::Trample,
            "Primal Rage", "{1}{G}", "Creatures you control have trample. (A creature with trample can deal excess combat damage to the player or planeswalker it's attacking.)"),
        ("Archetype of Endurance", 6, 5, "{6}{G}{G}", "Creatures you control have hexproof.\nCreatures your opponents control lose hexproof and can't have or gain hexproof.", Keyword::Hexproof,
            "Asceticism", "{3}{G}{G}", "Creatures you control have hexproof. (They can't be the targets of spells or abilities your opponents control.)\n{1}{G}: Regenerate target creature."),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let subtypes = match name {
            "Archetype of Courage" => vec!["Human", "Soldier"],
            "Archetype of Imagination" => vec!["Human", "Wizard"],
            "Archetype of Finality" => vec!["Gorgon"],
            "Archetype of Aggression" => vec!["Human", "Warrior"],
            "Archetype of Endurance" => vec!["Boar"],
            _ => unreachable!(),
        };
        let source = scenario.add_creature_from_oracle(P1, name, power, toughness, text)
            .as_enchantment().as_creature().with_subtypes(subtypes.clone()).with_mana_cost(complete_publication_cost(cost)).id();
        let opposing_copy = scenario.add_creature_to_hand_from_oracle(P0, name, power, toughness, text)
            .as_enchantment().as_creature().with_subtypes(subtypes).with_mana_cost(complete_publication_cost(cost)).id();
        let own = scenario.add_creature(P0, "Own Rat with printed keyword", 2, 3)
            .with_subtypes(vec!["Rat"]).with_keyword(keyword.clone()).id();
        let late = scenario.add_creature(P0, "Own Rat without printed keyword", 3, 4)
            .with_subtypes(vec!["Rat"]).id();
        let foreign = scenario.add_creature(P1, "Opposing Rat", 4, 5).with_subtypes(vec!["Rat"]).id();
        let grant = if grant_name == "Ogre Slumlord" {
            scenario.add_creature_to_hand_from_oracle(P0, grant_name, 3, 3, grant_text)
                .with_subtypes(vec!["Ogre", "Rogue"]).with_mana_cost(complete_publication_cost(grant_cost)).id()
        } else {
            scenario.add_spell_to_hand_from_oracle(P0, grant_name, false, grant_text)
                .as_enchantment().with_mana_cost(complete_publication_cost(grant_cost)).id()
        };
        let bounce = scenario.add_spell_to_hand_from_oracle(P0, "Disperse", true,
            "Return target nonland permanent to its owner's hand.")
            .with_mana_cost(complete_publication_cost("{1}{U}")).id();
        let steal = scenario.add_spell_to_hand_from_oracle(P0, "Turn Against", true,
            "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.")
            .with_mana_cost(complete_publication_cost("{4}{R}")).id();
        for player in [P0, P1] { for _ in 0..4 { scenario.add_card_to_library_top(player, "Draw buffer"); } }
        let mut runner = scenario.build();
        effective_pt(&mut runner, own);
        assert!(!runner.state().objects[&own].keywords.contains(&keyword), "{name}: remove existing");
        assert!(runner.state().objects[&foreign].keywords.contains(&keyword), "{name}: own grant positive");
        assert!(runner.state().objects[&source].keywords.contains(&keyword), "{name}: source included");
        // The later grant is a real paid permanent spell on its controller's turn.
        complete_publication_fund(&mut runner, P0, 8, &[ManaType::White, ManaType::White,
            ManaType::Blue, ManaType::Blue, ManaType::Black, ManaType::Black, ManaType::Green, ManaType::Green]);
        runner.cast(grant).resolve();
        assert_eq!(runner.state().objects[&grant].zone, Zone::Battlefield);
        assert!(!runner.state().objects[&late].keywords.contains(&keyword), "{name}: deny later grant");
        assert!(!runner.state().objects[&own].keywords.contains(&keyword), "{name}: cannot regrant");
        assert_continuous_complete(runner.state());
        let (generic, color) = match name {
            "Archetype of Courage" => (1, ManaType::White),
            "Archetype of Imagination" => (4, ManaType::Blue),
            "Archetype of Finality" => (4, ManaType::Black),
            "Archetype of Aggression" => (1, ManaType::Red),
            "Archetype of Endurance" => (6, ManaType::Green),
            _ => unreachable!(),
        };
        complete_publication_fund(&mut runner, P0, generic, &[color, color]);
        runner.cast(opposing_copy).resolve();
        for object in [source, opposing_copy, own, late, foreign] {
            assert!(!runner.state().objects[&object].keywords.contains(&keyword),
                "{name}: two real opposing sources deny both controllers");
        }
        assert_continuous_complete(runner.state());
        complete_publication_fund(&mut runner, P0, 4, &[ManaType::Red]);
        runner.cast(steal).target_object(source).resolve();
        assert_eq!(runner.state().objects[&source].owner, P1);
        assert_eq!(runner.state().objects[&source].controller, P0);
        for object in [source, opposing_copy, own, late] {
            assert!(runner.state().objects[&object].keywords.contains(&keyword), "{name}: live controller changes both grant and denial");
        }
        assert!(!runner.state().objects[&foreign].keywords.contains(&keyword));
        assert_continuous_complete(runner.state());
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
        runner.cast(bounce).target_object(source).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
        assert!(runner.state().objects[&late].keywords.contains(&keyword), "{name}: later hosted grant resumes");
        assert!(runner.state().objects[&own].keywords.contains(&keyword), "{name}: original ability resumes");
        assert!(!runner.state().objects[&foreign].keywords.contains(&keyword), "{name}: departed source grant ends");
        assert_eq!(effective_pt(&mut runner, late), (3, 4));
        assert_continuous_complete(runner.state());
        let departed_epoch = runner.state().objects[&source].incarnation;
        runner.advance_to_combat(); runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step(); runner.pass_both_players(); runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().active_player, P1);
        complete_publication_fund(&mut runner, P1, generic, &[color, color]);
        runner.cast(source).resolve();
        assert_ne!(runner.state().objects[&source].incarnation, departed_epoch);
        assert_eq!(runner.state().objects[&source].controller, P1);
        for object in [source, opposing_copy, own, late, foreign] {
            assert!(!runner.state().objects[&object].keywords.contains(&keyword), "{name}: real recast restores opposing denial");
        }
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_hosted_loss_preserves_same_instruction_defender() {
    for (name, cost, text, toughness, blue_wall) in [
        ("Deep Freeze", "{2}{U}", "Enchant creature\nEnchanted creature has base power and toughness 0/4, has defender, loses all other abilities, and is a blue Wall in addition to its other colors and types.", 4, true),
        ("Stasis Field", "{1}{U}", "Enchant creature\nEnchanted creature has base power and toughness 0/2, has defender, and loses all other abilities.", 2, false),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature_from_oracle(P1, "Archon of the Wild Rose", 4, 4, ARCHON)
            .with_subtypes(vec!["Archon"]).with_color(vec![ManaColor::White])
            .with_plus_counters(1).with_mana_cost(complete_publication_cost("{2}{W}{W}")).id();
        let unaffected = scenario.add_creature(P1, "Unaffected creature", 6, 7).with_keyword(Keyword::Flying).id();
        let aura = scenario.add_spell_to_hand_from_oracle(P0, name, false, text)
            .as_enchantment().with_subtypes(vec!["Aura"]).with_mana_cost(complete_publication_cost(cost)).id();
        let bounce = scenario.add_spell_to_hand_from_oracle(P0, "Disperse", true,
            "Return target nonland permanent to its owner's hand.")
            .with_mana_cost(complete_publication_cost("{1}{U}")).id();
        let mut runner = scenario.build();
        complete_publication_fund(&mut runner, P0, 3, &[ManaType::Blue, ManaType::Blue]);
        runner.cast(aura).target_object(target).resolve();
        assert_eq!(effective_pt(&mut runner, target), (1, toughness + 1));
        assert!(runner.state().objects[&target].keywords.contains(&Keyword::Defender));
        assert!(!runner.state().objects[&target].keywords.contains(&Keyword::Flying));
        assert!(runner.state().objects[&target].static_definitions.is_empty(), "printed anthem is removed");
        assert_eq!(runner.state().objects[&target].color.contains(&ManaColor::Blue), blue_wall);
        assert_eq!(runner.state().objects[&target].card_types.subtypes.iter().any(|t| t == "Wall"), blue_wall);
        assert!(runner.state().objects[&target].card_types.subtypes.iter().any(|t| t == "Archon"));
        assert_eq!(effective_pt(&mut runner, unaffected), (6, 7));
        assert!(runner.state().objects[&unaffected].keywords.contains(&Keyword::Flying));
        assert_continuous_complete(runner.state());
        runner.cast(bounce).target_object(aura).resolve();
        assert_eq!(effective_pt(&mut runner, target), (5, 5));
        assert!(runner.state().objects[&target].keywords.contains(&Keyword::Flying));
        assert!(!runner.state().objects[&target].keywords.contains(&Keyword::Defender));
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_paid_native_activations_preserve_companions_and_other_axis() {
    for (name, power, toughness, cost, text, subtypes, generic, colors, expected) in [
        ("Creeperhulk", 5, 5, "{3}{G}{G}", "Trample\n{1}{G}: Until end of turn, target creature you control has base power and toughness 5/5 and gains trample.", vec!["Plant", "Elemental"], 1, vec![ManaType::Green], (6, 6)),
        ("Turtle-Duck", 0, 4, "{G}", "{3}: Until end of turn, this creature has base power 4 and gains trample.", vec!["Turtle", "Bird"], 3, vec![], (5, 5)),
        ("Werewolf Pack Leader", 3, 3, "{G}{G}", "Pack tactics — Whenever this creature attacks, if you attacked with creatures with total power 6 or greater this combat, draw a card.\n{3}{G}: Until end of turn, this creature has base power and toughness 5/3, gains trample, and isn't a Human.", vec!["Human", "Werewolf"], 3, vec![ManaType::Green], (6, 4)),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_creature_from_oracle(P0, name, power, toughness, text)
            .with_subtypes(subtypes).with_mana_cost(complete_publication_cost(cost)).with_plus_counters(1).id();
        let ally = scenario.add_creature(P0, "Unequal ally", 2, 7).with_plus_counters(1).id();
        let foreign = scenario.add_creature(P1, "Foreign control", 6, 8).id();
        for player in [P0, P1] { for _ in 0..4 { scenario.add_card_to_library_top(player, "Turn buffer"); } }
        let mut runner = scenario.build();
        let recipient = if name == "Creeperhulk" { ally } else { source };
        assert!(runner.act(GameAction::ActivateAbility { payment_mode: engine::types::game_state::CastPaymentMode::Auto, source_id: source, ability_index: 0 })
            .is_err(), "unfunded activation {name}");
        complete_publication_fund(&mut runner, P0, generic, &colors);
        if name == "Creeperhulk" { runner.activate(source, 0).target_object(ally).resolve(); }
        else { runner.activate(source, 0).resolve(); }
        assert_eq!(effective_pt(&mut runner, recipient), expected, "{name}");
        assert!(runner.state().objects[&recipient].keywords.contains(&Keyword::Trample));
        if name == "Werewolf Pack Leader" {
            assert!(!runner.state().objects[&source].card_types.subtypes.iter().any(|t| t == "Human"));
            assert!(runner.state().objects[&source].card_types.subtypes.iter().any(|t| t == "Werewolf"));
        }
        assert_eq!(effective_pt(&mut runner, foreign), (6, 8));
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_continuous_complete(runner.state());
        runner.advance_to_combat(); runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step(); runner.pass_both_players();
        assert_eq!(effective_pt(&mut runner, recipient), if name == "Creeperhulk" { (3, 8) } else { (power + 1, toughness + 1) });
        assert!(!runner.state().objects[&recipient].keywords.contains(&Keyword::Trample));
        if name == "Werewolf Pack Leader" { assert!(runner.state().objects[&source].card_types.subtypes.iter().any(|t| t == "Human")); }
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_drop_bear_all_zone_current_and_departure() {
    for consumer in ["Bloom", "Scavenge", "Scavenge fizzle", "Swords"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(P0, ManaColor::Green);
        scenario
            .add_creature(P0, "Forest Bear", 1, 6)
            .as_land()
            .as_creature()
            .with_subtypes(vec!["Forest", "Bear"]);
        let recipient = scenario
            .add_creature(P0, "Other Bear", 2, 4)
            .with_subtypes(vec!["Bear"])
            .id();
        for _ in 0..5 {
            scenario.add_basic_land(P1, ManaColor::Green);
            scenario
                .add_creature(P1, "Foreign Bear", 2, 4)
                .with_subtypes(vec!["Bear"]);
        }
        let bear = scenario
            .add_creature_to_hand_from_oracle(P0, "Drop Bear", 0, 0, DROP_BEAR)
            .with_subtypes(vec!["Koala", "Bear", "Horror"])
            .with_mana_cost(complete_publication_cost("{3}{R}{G}"))
            .id();
        let remotes = [Zone::Library, Zone::Hand, Zone::Graveyard, Zone::Exile].map(|zone| {
            let id = scenario
                .add_creature_to_hand_from_oracle(P0, "Drop Bear", 0, 0, DROP_BEAR)
                .with_subtypes(vec!["Koala", "Bear", "Horror"])
                .with_mana_cost(complete_publication_cost("{3}{R}{G}"))
                .id();
            (id, zone)
        });
        let bloom = scenario
            .add_spell_to_hand_from_oracle(P0, "Morbid Bloom", false, BLOOM)
            .with_mana_cost(complete_publication_cost("{4}{B}{G}"))
            .id();
        scenario
            .add_creature_from_oracle(P0, "Varolz, the Scar-Striped", 2, 2, VAROLZ)
            .as_legendary()
            .with_subtypes(vec!["Troll", "Warrior"])
            .with_mana_cost(complete_publication_cost("{1}{B}{G}"));
        let growth = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Giant Growth",
                true,
                "Target creature gets +3/+3 until end of turn.",
            )
            .with_mana_cost(complete_publication_cost("{G}"))
            .id();
        let swords = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Swords to Plowshares",
                true,
                "Exile target creature. Its controller gains life equal to its power.",
            )
            .with_mana_cost(complete_publication_cost("{W}"))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{U}"))
            .id();
        let mut runner = scenario.build();
        for &(id, zone) in &remotes {
            if zone != Zone::Hand {
                engine::game::zones::move_to_zone(runner.state_mut(), id, zone, &mut vec![]);
            }
        }
        engine::game::layers::flush_layers(runner.state_mut());
        for &(id, zone) in &remotes {
            let object = &runner.state().objects[&id];
            assert_eq!(object.zone, zone);
            assert_eq!((object.power, object.toughness), (Some(4), Some(4)));
            assert_eq!(
                (object.layer_base_power, object.layer_base_toughness),
                (Some(4), Some(4))
            );
            assert_eq!(
                (object.base_power, object.base_toughness),
                (Some(0), Some(0))
            );
        }
        assert_continuous_complete(runner.state());
        if consumer == "Bloom" {
            let grave = remotes[2].0;
            complete_publication_fund(&mut runner, P0, 4, &[ManaType::Black, ManaType::Green]);
            runner.cast(bloom).target_object(grave).resolve();
            let tokens = runner
                .state()
                .objects
                .values()
                .filter(|object| object.is_token)
                .collect::<Vec<_>>();
            assert_eq!(tokens.len(), 4);
            for token in tokens {
                assert_eq!(token.zone, Zone::Battlefield);
                assert_eq!((token.owner, token.controller), (P0, P0));
                assert_eq!((token.power, token.toughness), (Some(1), Some(1)));
                assert_eq!(token.color, vec![ManaColor::Green]);
                assert_eq!(token.card_types.subtypes, vec!["Saproling".to_string()]);
            }
            assert_eq!(runner.state().objects[&grave].zone, Zone::Exile);
            assert_eq!(effective_pt(&mut runner, grave), (4, 4));
        } else if consumer == "Scavenge" || consumer == "Scavenge fizzle" {
            let grave = remotes[2].0;
            let (index, definition) =
                engine::game::casting::activated_ability_definitions(runner.state(), grave)
                    .into_iter()
                    .find(|(_, definition)| {
                        matches!(
                            definition.effect.as_ref(),
                            engine::types::ability::Effect::PutCounter { .. }
                        )
                    })
                    .expect("full Varolz supplies real paid Scavenge");
            assert_eq!(
                definition.cost,
                Some(engine::types::ability::AbilityCost::Composite {
                    costs: vec![
                        engine::types::ability::AbilityCost::Mana {
                            cost: complete_publication_cost("{3}{R}{G}")
                        },
                        engine::types::ability::AbilityCost::Exile {
                            count: 1,
                            zone: Some(Zone::Graveyard),
                            filter: Some(engine::types::ability::TargetFilter::SelfRef)
                        },
                    ]
                })
            );
            let epoch = runner.state().objects[&grave].incarnation;
            assert!(
                runner
                    .act(GameAction::ActivateAbility {
                        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                        source_id: grave,
                        ability_index: index
                    })
                    .is_err(),
                "the full 3RG Scavenge cost must actually be payable"
            );
            complete_publication_fund(&mut runner, P0, 3, &[ManaType::Red, ManaType::Green]);
            runner
                .act(GameAction::ActivateAbility {
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                    source_id: grave,
                    ability_index: index,
                })
                .unwrap();
            let WaitingFor::TargetSelection {
                player,
                target_slots,
                selection,
                pending_cast,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("required Scavenge target")
            };
            assert_eq!(*player, P0);
            assert_eq!(pending_cast.ability.source_incarnation, Some(epoch));
            assert!(target_slots[selection.current_slot]
                .legal_targets
                .contains(&TargetRef::Object(recipient)));
            assert!(target_slots[selection.current_slot].legal_targets.len() > 1);
            assert!(!target_slots[selection.current_slot]
                .legal_targets
                .contains(&TargetRef::Object(remotes[3].0)));
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(recipient)),
                })
                .unwrap();
            assert_eq!(runner.state().objects[&grave].zone, Zone::Exile);
            assert_eq!(
                runner
                    .state()
                    .stack
                    .last()
                    .unwrap()
                    .ability()
                    .unwrap()
                    .source_incarnation,
                Some(epoch)
            );
            if consumer == "Scavenge fizzle" {
                complete_publication_fund(&mut runner, P0, 0, &[ManaType::Blue]);
                runner.cast(bounce).target_object(recipient).commit();
                runner.resolve_top();
                assert_eq!(runner.state().objects[&recipient].zone, Zone::Hand);
                assert_eq!(
                    runner.state().stack.len(),
                    1,
                    "the real paid Scavenge is still below the removal"
                );
                runner.advance_until_stack_empty();
                assert!(!runner.state().objects[&recipient]
                    .counters
                    .contains_key(&CounterType::Plus1Plus1));
                assert_eq!(effective_pt(&mut runner, recipient), (2, 4));
            } else {
                runner.advance_until_stack_empty();
                assert_eq!(
                    runner.state().objects[&recipient]
                        .counters
                        .get(&CounterType::Plus1Plus1),
                    Some(&4)
                );
            }
            assert_eq!(
                effective_pt(&mut runner, grave),
                if consumer == "Scavenge fizzle" {
                    (3, 3)
                } else {
                    (4, 4)
                }
            );
        } else {
            complete_publication_fund(&mut runner, P0, 3, &[ManaType::Red, ManaType::Green]);
            runner.cast(bear).commit();
            assert_eq!(runner.state().objects[&bear].zone, Zone::Stack);
            assert_eq!(effective_pt(&mut runner, bear), (4, 4));
            runner.pass_both_players();
            let WaitingFor::TriggerTargetSelection {
                player,
                source_id,
                target_slots,
                selection,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("required actual ETB fight")
            };
            assert_eq!(*player, P0);
            assert_eq!(*source_id, Some(bear));
            assert!(target_slots[selection.current_slot]
                .legal_targets
                .contains(&TargetRef::Object(recipient)));
            runner
                .act(GameAction::ChooseTarget { target: None })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_eq!(effective_pt(&mut runner, bear), (5, 5));
            assert_continuous_complete(runner.state());
            complete_publication_fund(&mut runner, P0, 0, &[ManaType::Green]);
            runner.cast(growth).target_object(bear).resolve();
            assert_eq!(effective_pt(&mut runner, bear), (8, 8));
            assert_eq!(runner.state().objects[&bear].layer_base_power, Some(5));
            assert_continuous_complete(runner.state());
            complete_publication_fund(&mut runner, P0, 0, &[ManaType::White]);
            runner
                .cast(swords)
                .target_object(bear)
                .resolve()
                .assert_life_delta(P0, 8);
            assert_eq!(runner.state().objects[&bear].zone, Zone::Exile);
            assert_eq!(effective_pt(&mut runner, bear), (4, 4));
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_aurelia_colors_latch_and_target_incarnation_is_pinned() {
    for blink in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let aurelia = scenario
            .add_creature_from_oracle(P0, "Aurelia, Exemplar of Justice", 2, 5, AURELIA)
            .as_legendary()
            .with_color(vec![ManaColor::Red, ManaColor::White])
            .with_subtypes(vec!["Angel"])
            .with_mana_cost(complete_publication_cost("{2}{R}{W}"))
            .id();
        let chosen = scenario
            .add_creature(P0, "Chosen", 2, 2)
            .with_color(vec![ManaColor::Red])
            .id();
        let decoy = scenario
            .add_creature(P0, "Other choice", 3, 3)
            .with_color(vec![ManaColor::White])
            .id();
        let lenses = [0, 1].map(|_| {
            scenario
                .add_artifact_from_oracle(P0, "Distorting Lens", LENS)
                .with_mana_cost(complete_publication_cost("{2}"))
                .id()
        });
        let cloudshift = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true,
            "Exile target creature you control, then return that card to the battlefield under your control.")
            .with_mana_cost(complete_publication_cost("{W}")).id();
        let mut runner = scenario.build();
        runner.advance_to_phase(Phase::BeginCombat);
        let WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Aurelia trigger target")
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, Some(aurelia));
        assert_eq!(target_slots.len(), 1);
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(chosen)));
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(decoy)));
        let epoch = runner.state().objects[&chosen].incarnation;
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(chosen)),
            })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        if blink {
            complete_publication_fund(&mut runner, P0, 0, &[ManaType::White]);
            runner.cast(cloudshift).target_object(chosen).resolve();
            assert_ne!(runner.state().objects[&chosen].incarnation, epoch);
            assert_eq!(effective_pt(&mut runner, chosen), (2, 2));
            assert!(!runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Trample));
            assert!(!runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Vigilance));
        } else {
            for (index, color) in [(0, "White"), (1, "Red")] {
                runner
                    .act(GameAction::ActivateAbility {
                        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                        source_id: lenses[index],
                        ability_index: 0,
                    })
                    .unwrap();
                let WaitingFor::TargetSelection {
                    player,
                    target_slots,
                    selection,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("Lens target")
                };
                assert_eq!(*player, P0);
                assert_eq!(target_slots.len(), 1);
                assert!(target_slots[selection.current_slot]
                    .legal_targets
                    .contains(&TargetRef::Object(chosen)));
                assert!(target_slots[selection.current_slot]
                    .legal_targets
                    .contains(&TargetRef::Object(decoy)));
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(chosen)),
                    })
                    .unwrap();
                runner.resolve_top();
                let WaitingFor::NamedChoice {
                    player, options, ..
                } = &runner.state().waiting_for
                else {
                    panic!("Lens color")
                };
                assert_eq!(*player, P0);
                assert_eq!(
                    options,
                    &vec![
                        "White".to_string(),
                        "Blue".into(),
                        "Black".into(),
                        "Red".into(),
                        "Green".into()
                    ]
                );
                runner
                    .act(GameAction::ChooseOption {
                        choice: color.into(),
                    })
                    .unwrap();
                if index == 0 {
                    // This is an intermediate Lens completion: Aurelia still waits below it.
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(runner.state().resolution_stack.is_empty());
                    assert_eq!(runner.state().stack.len(), 1);
                    assert_eq!(
                        runner.state().stack[0].ability().unwrap().source_id,
                        aurelia
                    );
                    assert_eq!(effective_pt(&mut runner, chosen), (2, 2));
                    runner.advance_until_stack_empty();
                }
                assert_eq!(effective_pt(&mut runner, chosen), (4, 2));
                assert!(runner.state().objects[&chosen]
                    .keywords
                    .contains(&Keyword::Vigilance));
                assert!(!runner.state().objects[&chosen]
                    .keywords
                    .contains(&Keyword::Trample));
                assert_continuous_complete(runner.state());
            }
            assert_eq!(runner.state().objects[&chosen].color, vec![ManaColor::Red]);
        }
        assert_eq!(effective_pt(&mut runner, decoy), (3, 3));
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        assert_eq!(effective_pt(&mut runner, chosen), (2, 2));
        assert!(!runner.state().objects[&chosen]
            .keywords
            .contains(&Keyword::Vigilance));
        assert!(!runner.state().objects[&chosen]
            .keywords
            .contains(&Keyword::Trample));
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_aurelia_empty_legal_targets_uses_real_greaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aurelia = scenario
        .add_creature_from_oracle(P0, "Aurelia, Exemplar of Justice", 2, 5, AURELIA)
        .as_legendary()
        .with_subtypes(vec!["Angel"])
        .with_color(vec![ManaColor::Red, ManaColor::White])
        .with_mana_cost(complete_publication_cost("{2}{R}{W}"))
        .id();
    let greaves = scenario
        .add_artifact_from_oracle(P0, "Lightning Greaves", GREAVES)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(complete_publication_cost("{2}"))
        .id();
    scenario.add_creature(P1, "Foreign target", 2, 2);
    let mut runner = scenario.build();
    let mut legal_twin = GameRunner::from_state(runner.state().clone());
    legal_twin.advance_to_phase(Phase::BeginCombat);
    let WaitingFor::TriggerTargetSelection {
        player,
        source_id,
        target_slots,
        selection,
        ..
    } = &legal_twin.state().waiting_for
    else {
        panic!("unshrouded positive trigger");
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, Some(aurelia));
    assert_eq!(
        target_slots[selection.current_slot].legal_targets,
        vec![TargetRef::Object(aurelia)]
    );
    legal_twin
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(aurelia)),
        })
        .unwrap();
    legal_twin.advance_until_stack_empty();
    assert_eq!(effective_pt(&mut legal_twin, aurelia), (4, 5));
    assert!(legal_twin.state().objects[&aurelia]
        .keywords
        .contains(&Keyword::Trample));
    assert!(legal_twin.state().objects[&aurelia]
        .keywords
        .contains(&Keyword::Vigilance));
    assert_continuous_complete(legal_twin.state());
    runner
        .act(GameAction::Equip {
            equipment_id: greaves,
            target_id: aurelia,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&aurelia]
        .keywords
        .contains(&Keyword::Shroud));
    assert_continuous_complete(runner.state());
    runner.advance_to_phase(Phase::BeginCombat);
    // The optional target declaration is satisfiable with zero choices.
    runner.advance_until_stack_empty();
    assert_eq!(effective_pt(&mut runner, aurelia), (2, 5));
    assert!(!runner.state().objects[&aurelia]
        .keywords
        .contains(&Keyword::Trample));
    assert!(!runner.state().objects[&aurelia]
        .keywords
        .contains(&Keyword::Vigilance));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_aurelia_two_real_occurrences_choose_distinct_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Aurelia, Exemplar of Justice", 2, 5, AURELIA)
        .as_legendary()
        .with_subtypes(vec!["Angel"])
        .with_color(vec![ManaColor::Red, ManaColor::White])
        .with_mana_cost(complete_publication_cost("{2}{R}{W}"))
        .id();
    let first = scenario
        .add_creature(P0, "First red recipient", 2, 2)
        .with_color(vec![ManaColor::Red])
        .id();
    let second = scenario
        .add_creature(P0, "Second white recipient", 3, 4)
        .with_color(vec![ManaColor::White])
        .id();
    let entrant = scenario
        .add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(complete_publication_cost("{1}{G}"))
        .id();
    let blink = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true,
        "Exile target creature you control, then return that card to the battlefield under your control.")
        .with_mana_cost(complete_publication_cost("{W}")).id();
    for player in [P0, P1] {
        for _ in 0..8 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    let mut epochs = Vec::new();
    for (occurrence, chosen, expected) in [(0, first, (4, 2)), (1, second, (5, 4))] {
        assert_eq!(runner.state().active_player, P0);
        runner.advance_to_phase(Phase::BeginCombat);
        let WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real occurrence {occurrence}");
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, Some(source));
        assert_eq!(target_slots.len(), 1);
        assert!(target_slots[selection.current_slot].optional);
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(first)));
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(second)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(chosen)),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        epochs.push(runner.state().objects[&source].incarnation);
        assert_eq!(effective_pt(&mut runner, chosen), expected);
        assert_eq!(
            runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Trample),
            occurrence == 0
        );
        assert_eq!(
            runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Vigilance),
            occurrence == 1
        );
        assert_eq!(
            effective_pt(&mut runner, if occurrence == 0 { second } else { first }),
            if occurrence == 0 { (3, 4) } else { (2, 2) }
        );
        if occurrence == 1 {
            assert_eq!(effective_pt(&mut runner, entrant), (2, 2));
        }
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_phase(Phase::PostCombatMain);
        if occurrence == 0 {
            complete_publication_fund(&mut runner, P0, 1, &[ManaType::Green, ManaType::White]);
            runner.cast(entrant).resolve();
            assert_eq!(effective_pt(&mut runner, entrant), (2, 2));
            assert!(!runner.state().objects[&entrant]
                .keywords
                .contains(&Keyword::Trample));
            runner.cast(blink).target_object(source).resolve();
            assert_eq!(
                effective_pt(&mut runner, first),
                (4, 2),
                "resolved grant survives source reentry"
            );
            assert_continuous_complete(runner.state());
        }
        runner.advance_to_end_step();
        runner.pass_both_players();
        assert_eq!(effective_pt(&mut runner, first), (2, 2));
        assert_eq!(effective_pt(&mut runner, second), (3, 4));
        assert!(!runner.state().objects[&first]
            .keywords
            .contains(&Keyword::Trample));
        assert!(!runner.state().objects[&second]
            .keywords
            .contains(&Keyword::Vigilance));
        assert_continuous_complete(runner.state());
        if occurrence == 0 {
            runner.advance_to_phase(Phase::PreCombatMain);
            assert_eq!(runner.state().active_player, P1);
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            runner.pass_both_players();
            runner.advance_to_phase(Phase::PreCombatMain);
            assert_eq!(runner.state().active_player, P0);
            assert_continuous_complete(runner.state());
        }
    }
    assert_ne!(
        epochs[0], epochs[1],
        "second event uses returned source incarnation"
    );
}

#[test]
fn complete_continuous_aurelia_control_changes_preserve_the_trigger_and_latched_recipient() {
    for (steal_source, after_resolution) in
        [(true, false), (false, false), (true, true), (false, true)]
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(P0, "Aurelia, Exemplar of Justice", 2, 5, AURELIA)
            .as_legendary()
            .with_subtypes(vec!["Angel"])
            .with_color(vec![ManaColor::Red, ManaColor::White])
            .with_mana_cost(complete_publication_cost("{2}{R}{W}"))
            .id();
        let recipient = scenario
            .add_creature(P0, "White recipient", 2, 2)
            .with_color(vec![ManaColor::White])
            .id();
        let other = scenario
            .add_creature(P0, "Untouched red recipient", 6, 7)
            .with_color(vec![ManaColor::Red])
            .id();
        let theft = scenario.add_spell_to_hand_from_oracle(P1, "Turn Against", true,
            "Devoid (This card has no color.)\nGain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.")
            .with_mana_cost(complete_publication_cost("{4}{R}")).id();
        for player in [P0, P1] {
            for _ in 0..4 {
                scenario.add_card_to_library_top(player, "Turn buffer");
            }
        }
        let mut runner = scenario.build();
        runner.advance_to_phase(Phase::BeginCombat);
        let WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual BeginCombat target");
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, Some(source));
        assert_eq!(target_slots.len(), 1);
        assert!(target_slots[selection.current_slot].optional);
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(recipient)));
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(other)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(recipient)),
            })
            .unwrap();
        if after_resolution {
            runner.advance_until_stack_empty();
            assert_eq!(effective_pt(&mut runner, recipient), (4, 2));
            assert!(runner.state().objects[&recipient]
                .keywords
                .contains(&Keyword::Vigilance));
            assert_continuous_complete(runner.state());
        }
        complete_publication_fund(&mut runner, P1, 4, &[ManaType::Red]);
        runner.act(GameAction::PassPriority).unwrap();
        let stolen = if steal_source { source } else { recipient };
        runner.cast(theft).target_object(stolen).resolve();
        assert_eq!(runner.state().objects[&stolen].controller, P1);
        assert_eq!(runner.state().objects[&stolen].owner, P0);
        let applies = steal_source || after_resolution;
        // The announced target is revalidated; a completed transient keeps its recipient.
        assert_eq!(
            effective_pt(&mut runner, recipient),
            if applies { (4, 2) } else { (2, 2) }
        );
        assert_eq!(
            runner.state().objects[&recipient]
                .keywords
                .contains(&Keyword::Vigilance),
            applies
        );
        assert!(!runner.state().objects[&recipient]
            .keywords
            .contains(&Keyword::Trample));
        assert_eq!(effective_pt(&mut runner, other), (6, 7));
        assert!(!runner.state().objects[&other]
            .keywords
            .contains(&Keyword::Trample));
        assert_eq!(runner.state().players[1].mana_pool.total(), 0);
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        assert_eq!(runner.state().objects[&stolen].controller, P0);
        assert_eq!(effective_pt(&mut runner, recipient), (2, 2));
        assert!(!runner.state().objects[&recipient]
            .keywords
            .contains(&Keyword::Vigilance));
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_water_wings_and_gifter_use_paid_spell_and_real_entry() {
    for (gifter, decline) in [(false, false), (true, false), (true, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let chosen = scenario
            .add_creature(P0, "Chosen creature", 2, 7)
            .with_plus_counters(1)
            .id();
        let other = scenario.add_creature(P0, "Other creature", 6, 8).id();
        let foreign = scenario.add_creature(P1, "Foreign creature", 8, 6).id();
        let spell = if gifter {
            scenario.add_creature_to_hand_from_oracle(P0, "Glamer Gifter", 1, 2,
                "Flash\nFlying\nWhen this creature enters, choose up to one other target creature. Until end of turn, that creature has base power and toughness 4/4 and gains all creature types.")
                .with_subtypes(vec!["Faerie", "Wizard"]).with_mana_cost(complete_publication_cost("{1}{U}")).id()
        } else {
            scenario.add_spell_to_hand_from_oracle(P0, "Water Wings", true,
                "Until end of turn, target creature you control has base power and toughness 4/4 and gains flying and hexproof. (It can't be the target of spells or abilities your opponents control.)")
                .with_mana_cost(complete_publication_cost("{1}{U}")).id()
        };
        let mut runner = scenario.build();
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
        if gifter {
            runner.cast(spell).commit();
            runner.pass_both_players();
            let WaitingFor::TriggerTargetSelection {
                player,
                source_id,
                target_slots,
                selection,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("Gifter ETB target");
            };
            assert_eq!(*player, P0);
            assert_eq!(*source_id, Some(spell));
            assert_eq!(target_slots.len(), 1);
            let slot = &target_slots[selection.current_slot];
            assert!(slot.optional);
            for target in [chosen, other, foreign] {
                assert!(slot.legal_targets.contains(&TargetRef::Object(target)));
            }
            assert!(
                !slot.legal_targets.contains(&TargetRef::Object(spell)),
                "other excludes actual ETB source"
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: if decline {
                        None
                    } else {
                        Some(TargetRef::Object(chosen))
                    },
                })
                .unwrap();
            runner.advance_until_stack_empty();
        } else {
            runner.cast(spell).target_object(chosen).resolve();
        }
        assert_eq!(
            effective_pt(&mut runner, chosen),
            if decline { (3, 8) } else { (5, 5) }
        );
        assert_eq!(
            runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Changeling),
            gifter && !decline
        );
        assert_eq!(
            runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Flying),
            !gifter
        );
        assert_eq!(
            runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Hexproof),
            !gifter
        );
        assert_eq!(effective_pt(&mut runner, other), (6, 8));
        assert_eq!(effective_pt(&mut runner, foreign), (8, 6));
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_continuous_complete(runner.state());
    }
}

fn complete_publication_cost(text: &str) -> ManaCost {
    all_consuming(parse_mana_cost).parse(text).unwrap().1
}

fn complete_publication_pool(generic: usize, colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .copied()
        .chain(std::iter::repeat_n(ManaType::Colorless, generic))
        .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
        .collect()
}

fn assert_continuous_complete(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

fn complete_publication_fund(
    runner: &mut GameRunner,
    player: PlayerId,
    generic: usize,
    colors: &[ManaType],
) {
    for mana in complete_publication_pool(generic, colors) {
        runner.state_mut().add_mana_to_pool(player, mana).unwrap();
    }
}

#[test]
fn complete_continuous_members_reach_layers_and_end_with_aura_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ally = scenario.add_creature(P0, "Host", 2, 3).id();
    let foreign = scenario.add_creature(P1, "Foreign", 7, 8).id();
    let aura = scenario
        .add_spell_to_hand_from_oracle(P0, "Almost Perfect", false, ALMOST)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(complete_publication_cost("{4}{G}{W}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    scenario.with_mana_pool(
        P0,
        complete_publication_pool(5, &[ManaType::Green, ManaType::White, ManaType::Blue]),
    );
    let mut runner = scenario.build();
    runner.cast(aura).target_object(ally).resolve();
    // CR 613.4b + CR 613.1f: both base axes AND the keyword must survive.
    assert_eq!(effective_pt(&mut runner, ally), (9, 10));
    assert!(runner.state().objects[&ally]
        .keywords
        .contains(&Keyword::Indestructible));
    assert_eq!(effective_pt(&mut runner, foreign), (7, 8));
    assert!(!runner.state().objects[&foreign]
        .keywords
        .contains(&Keyword::Indestructible));
    assert_continuous_complete(runner.state());
    runner.cast(bounce).target_object(aura).resolve();
    assert_eq!(runner.state().objects[&aura].zone, Zone::Hand);
    // CR 611.3a: the live hosted effect ends with its source.
    assert_eq!(effective_pt(&mut runner, ally), (2, 3));
    assert!(!runner.state().objects[&ally]
        .keywords
        .contains(&Keyword::Indestructible));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_equipment_companions_follow_their_own_source() {
    for (name, text, counter, keyword, subtype) in [
        (
            "Mace of the Valiant",
            MACE,
            CounterType::Generic("charge".into()),
            Some(Keyword::Vigilance),
            None,
        ),
        (
            "Reaper's Scythe",
            SCYTHE,
            CounterType::Generic("soul".into()),
            None,
            Some("Assassin"),
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_artifact_from_oracle(P0, name, text)
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(complete_publication_cost(
                if name == "Mace of the Valiant" {
                    "{2}{W}"
                } else {
                    "{2}{B}"
                },
            ))
            .id();
        let other_source = scenario
            .add_artifact_from_oracle(P0, name, text)
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(complete_publication_cost(
                if name == "Mace of the Valiant" {
                    "{2}{W}"
                } else {
                    "{2}{B}"
                },
            ))
            .id();
        scenario.with_counter(source, counter.clone(), 2);
        scenario.with_counter(source, CounterType::Generic("unrelated".into()), 7);
        scenario.with_counter(other_source, counter, 5);
        let first = scenario.add_creature(P0, "First host", 2, 3).id();
        let second = scenario.add_creature(P0, "Second host", 4, 5).id();
        let opponent = scenario.add_creature(P1, "Illegal foreign host", 6, 7).id();
        scenario.with_mana_pool(P0, complete_publication_pool(20, &[]));
        let mut runner = scenario.build();
        assert!(runner
            .act(GameAction::Equip {
                equipment_id: source,
                target_id: opponent
            })
            .is_err());
        runner
            .act(GameAction::Equip {
                equipment_id: source,
                target_id: first,
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(effective_pt(&mut runner, first), (4, 5));
        if let Some(keyword) = &keyword {
            assert!(runner.state().objects[&first].keywords.contains(keyword));
        }
        if let Some(subtype) = subtype {
            assert!(runner.state().objects[&first]
                .card_types
                .subtypes
                .iter()
                .any(|s| s == subtype));
        }
        assert_eq!(effective_pt(&mut runner, second), (4, 5));
        assert_continuous_complete(runner.state());
        runner
            .act(GameAction::Equip {
                equipment_id: other_source,
                target_id: second,
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(effective_pt(&mut runner, second), (9, 10));
        runner
            .act(GameAction::Equip {
                equipment_id: source,
                target_id: second,
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(effective_pt(&mut runner, first), (2, 3));
        assert_eq!(effective_pt(&mut runner, second), (11, 12));
        if let Some(keyword) = &keyword {
            assert!(!runner.state().objects[&first].keywords.contains(keyword));
        }
        if let Some(subtype) = subtype {
            assert!(!runner.state().objects[&first]
                .card_types
                .subtypes
                .iter()
                .any(|s| s == subtype));
        }
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_entity_activation_preserves_both_base_axes_and_types() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let entity = scenario
        .add_creature_from_oracle(P0, "Mirror Entity", 1, 1, ENTITY)
        .id();
    let ally = scenario
        .add_creature(P0, "Ally", 2, 3)
        .with_subtypes(vec!["Bear"])
        .id();
    let foreign = scenario.add_creature(P1, "Foreign", 6, 7).id();
    scenario.with_mana_pool(P0, complete_publication_pool(4, &[]));
    let mut runner = scenario.build();
    runner.activate(entity, 0).x(4).resolve();
    for id in [entity, ally] {
        assert_eq!(effective_pt(&mut runner, id), (4, 4));
        assert!(runner.state().objects[&id]
            .keywords
            .contains(&Keyword::Changeling));
        assert!(runner.state().objects[&id].card_types.subtypes.len() > 20);
    }
    assert_eq!(effective_pt(&mut runner, foreign), (6, 7));
    assert!(!runner.state().objects[&foreign]
        .keywords
        .contains(&Keyword::Changeling));
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.pass_both_players();
    assert_eq!(effective_pt(&mut runner, ally), (2, 3));
    assert!(!runner.state().objects[&ally]
        .keywords
        .contains(&Keyword::Changeling));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_qualified_counts_use_actual_population_and_source_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jodah = scenario
        .add_creature_to_hand_from_oracle(P0, "Jodah, the Unifier", 5, 5, JODAH)
        .as_legendary()
        .with_mana_cost(complete_publication_cost("{W}{U}{B}{R}{G}"))
        .id();
    let legendary = scenario
        .add_creature(P0, "Legendary ally", 2, 2)
        .as_legendary()
        .id();
    let ordinary = scenario.add_creature(P0, "Ordinary ally", 2, 2).id();
    let foreign = scenario
        .add_creature(P1, "Foreign legend", 7, 7)
        .as_legendary()
        .id();
    let hancock = scenario
        .add_creature_to_hand_from_oracle(P0, "Hancock, Ghoulish Mayor", 2, 1, HANCOCK)
        .as_legendary()
        .with_subtypes(vec!["Zombie", "Mutant"])
        .with_mana_cost(complete_publication_cost("{2}{B}"))
        .id();
    let zombie = scenario
        .add_creature(P0, "Zombie", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();
    let mutant = scenario
        .add_creature(P0, "Mutant", 2, 2)
        .with_subtypes(vec!["Mutant"])
        .id();
    let both = scenario
        .add_creature(P0, "Both", 2, 2)
        .with_subtypes(vec!["Zombie", "Mutant"])
        .id();
    let foreign_zombie = scenario
        .add_creature(P1, "Foreign Zombie", 3, 3)
        .with_subtypes(vec!["Zombie"])
        .id();
    let donor = scenario.add_creature(P0, "Counter donor", 2, 3).id();
    scenario.with_counter(donor, CounterType::Generic("charge".into()), 2);
    scenario.with_counter(donor, CounterType::Generic("blood".into()), 1);
    let transfer = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Fate Transfer",
            true,
            "Move all counters from target creature onto another target creature.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U/B}"))
        .id();
    let remove = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Fate Transfer",
            true,
            "Move all counters from target creature onto another target creature.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U/B}"))
        .id();
    scenario.with_counter(zombie, CounterType::Generic("charge".into()), 7);
    scenario.with_mana_pool(
        P0,
        complete_publication_pool(
            3,
            &[
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
                ManaType::Black,
                ManaType::Blue,
            ],
        ),
    );
    let mut runner = scenario.build();
    runner.cast(hancock).resolve();
    assert_eq!(effective_pt(&mut runner, hancock), (2, 1));
    for object in [zombie, mutant, both] {
        assert_eq!(
            effective_pt(&mut runner, object),
            (2, 2),
            "zero source counters grant no numeric bonus"
        );
    }
    assert_continuous_complete(runner.state());
    runner.cast(jodah).resolve();
    assert_continuous_complete(runner.state());
    runner
        .cast(transfer)
        .target_objects(&[donor, hancock])
        .resolve();
    assert!(runner.state().objects[&donor].counters.is_empty());
    assert_eq!(
        runner.state().objects[&hancock]
            .counters
            .get(&CounterType::Generic("charge".into())),
        Some(&2)
    );
    assert_eq!(
        runner.state().objects[&hancock]
            .counters
            .get(&CounterType::Generic("blood".into())),
        Some(&1)
    );
    // CR 109.4: only controlled legendary creatures enter Jodah's live count.
    assert_eq!(effective_pt(&mut runner, jodah), (8, 8));
    assert_eq!(effective_pt(&mut runner, legendary), (5, 5));
    assert_eq!(effective_pt(&mut runner, ordinary), (2, 2));
    assert_eq!(effective_pt(&mut runner, foreign), (7, 7));
    // CR 122.1: all SOURCE counters; Zombie-or-Mutant membership counts once.
    for id in [zombie, mutant, both] {
        assert_eq!(effective_pt(&mut runner, id), (5, 5));
    }
    assert_eq!(effective_pt(&mut runner, hancock), (5, 4));
    assert_eq!(effective_pt(&mut runner, foreign_zombie), (3, 3));
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner
        .cast(remove)
        .target_objects(&[hancock, donor])
        .resolve();
    assert!(runner.state().objects[&hancock].counters.is_empty());
    assert_eq!(
        runner.state().objects[&donor]
            .counters
            .get(&CounterType::Generic("charge".into())),
        Some(&2)
    );
    assert_eq!(
        runner.state().objects[&donor]
            .counters
            .get(&CounterType::Generic("blood".into())),
        Some(&1)
    );
    for id in [zombie, mutant, both] {
        assert_eq!(
            effective_pt(&mut runner, id),
            (2, 2),
            "live source counter removal ends the whole union bonus"
        );
    }
    assert_eq!(effective_pt(&mut runner, hancock), (5, 4));
    assert_eq!(effective_pt(&mut runner, jodah), (8, 8));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_zinnia_reads_base_power_after_setters_before_additive_effects() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zinnia = scenario
        .add_creature_to_hand_from_oracle(P0, "Zinnia, Valley's Voice", 1, 3, ZINNIA)
        .as_legendary()
        .with_mana_cost(complete_publication_cost("{U}{R}{W}"))
        .id();
    let base_one = scenario
        .add_creature(P0, "Base one currently three", 1, 4)
        .with_plus_counters(2)
        .id();
    let base_two = scenario
        .add_creature(P0, "Base two currently one", 2, 4)
        .with_minus_counters(1)
        .id();
    scenario.add_creature(P1, "Foreign base one", 1, 4);
    let set_one = scenario.add_creature(P0, "Setter recipient", 5, 5).id();
    let maro = scenario
        .add_creature_from_oracle(
            P0,
            "Maro",
            0,
            0,
            "Maro's power and toughness are each equal to the number of cards in your hand.",
        )
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(complete_publication_cost("{2}{G}{G}"))
        .id();
    let frog = scenario.add_spell_to_hand_from_oracle(P0, "Turn to Frog", true,
        "Until end of turn, target creature loses all abilities and becomes a blue Frog with base power and toughness 1/1.")
        .with_mana_cost(complete_publication_cost("{1}{U}")).id();
    scenario.add_card_to_hand(P0, "Hand count witness");
    scenario.with_mana_pool(
        P0,
        complete_publication_pool(0, &[ManaType::Blue, ManaType::Red, ManaType::White]),
    );
    let mut runner = scenario.build();
    runner.cast(zinnia).resolve();
    assert_continuous_complete(runner.state());
    assert_eq!(effective_pt(&mut runner, maro), (2, 2));
    assert_eq!(effective_pt(&mut runner, zinnia), (2, 3));
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(frog).target_object(set_one).resolve();
    assert_eq!(effective_pt(&mut runner, base_one), (3, 6));
    assert_eq!(effective_pt(&mut runner, base_two), (1, 3));
    assert_eq!(effective_pt(&mut runner, set_one), (1, 1));
    assert_eq!(effective_pt(&mut runner, maro), (1, 1));
    // CR 208.4b: ignore additive modifiers/counters, include base setters; exclude self.
    assert_eq!(effective_pt(&mut runner, zinnia), (4, 3));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_aurelia_four_colors_decline_and_departing_source() {
    for colors in [
        vec![],
        vec![ManaColor::Red],
        vec![ManaColor::White],
        vec![ManaColor::Red, ManaColor::White],
    ] {
        for (decline, departing_source) in [(false, false), (true, false), (false, true)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_creature_from_oracle(P0, "Aurelia, Exemplar of Justice", 2, 5, AURELIA)
                .as_legendary()
                .with_subtypes(vec!["Angel"])
                .with_mana_cost(complete_publication_cost("{2}{R}{W}"))
                .with_color(vec![ManaColor::Red, ManaColor::White])
                .id();
            let chosen = scenario
                .add_creature(P0, "Chosen target", 2, 2)
                .with_color(colors.clone())
                .id();
            let other = scenario
                .add_creature(P0, "Other own target", 6, 6)
                .with_color(vec![ManaColor::Red, ManaColor::White])
                .id();
            let foreign = scenario
                .add_creature(P1, "Foreign target", 2, 2)
                .with_color(vec![ManaColor::Red, ManaColor::White])
                .id();
            let removal = scenario
                .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
                .with_mana_cost(complete_publication_cost("{1}{B}{B}"))
                .id();
            for player in [P0, P1] {
                for _ in 0..4 {
                    scenario.add_card_to_library_top(player, "Draw buffer");
                }
            }
            let mut runner = scenario.build();
            // The ordinary phase transition produces the actual source trigger.
            runner.advance_to_phase(Phase::BeginCombat);
            let WaitingFor::TriggerTargetSelection {
                player,
                source_id,
                target_slots,
                selection,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "required Aurelia target prompt: {:?}",
                    runner.state().waiting_for
                )
            };
            assert_eq!(*player, P0);
            assert_eq!(*source_id, Some(source));
            assert_eq!(target_slots.len(), 1);
            let slot = &target_slots[selection.current_slot];
            assert!(slot.legal_targets.contains(&TargetRef::Object(chosen)));
            assert!(slot.legal_targets.contains(&TargetRef::Object(other)));
            assert!(!slot.legal_targets.contains(&TargetRef::Object(foreign)));
            runner
                .act(GameAction::ChooseTarget {
                    target: (!decline).then_some(TargetRef::Object(chosen)),
                })
                .unwrap();
            if departing_source {
                complete_publication_fund(&mut runner, P0, 1, &[ManaType::Black, ManaType::Black]);
                runner.cast(removal).target_object(source).resolve();
            } else {
                runner.advance_until_stack_empty();
            }
            // CR 608.2c + CR 611.2c: exactly one unconditional pump, followed
            // by two independent resolution-time recipient color decisions.
            assert_eq!(
                effective_pt(&mut runner, chosen),
                if decline { (2, 2) } else { (4, 2) }
            );
            assert_eq!(
                runner.state().objects[&chosen]
                    .keywords
                    .contains(&Keyword::Trample),
                !decline && colors.contains(&ManaColor::Red)
            );
            assert_eq!(
                runner.state().objects[&chosen]
                    .keywords
                    .contains(&Keyword::Vigilance),
                !decline && colors.contains(&ManaColor::White)
            );
            assert_eq!(effective_pt(&mut runner, other), (6, 6));
            assert!(!runner.state().objects[&other]
                .keywords
                .contains(&Keyword::Trample));
            assert_eq!(effective_pt(&mut runner, foreign), (2, 2));
            if departing_source {
                assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            } else {
                assert_eq!(effective_pt(&mut runner, source), (2, 5));
            }
            assert_continuous_complete(runner.state());
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            runner.pass_both_players();
            // CR 611.2a: every installed member expires at the stated cleanup.
            assert_eq!(effective_pt(&mut runner, chosen), (2, 2));
            assert!(!runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Trample));
            assert!(!runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Vigilance));
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn complete_continuous_drop_bear_sum_cda_paid_entry_and_fight_choices() {
    for (fight, victim_controller) in [(false, P1), (true, P1), (true, P0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(P0, ManaColor::Green);
        // A Forest Bear contributes to BOTH independent summands.
        scenario
            .add_creature(P0, "Forest Bear", 1, 6)
            .as_land()
            .as_creature()
            .with_subtypes(vec!["Forest", "Bear"]);
        scenario
            .add_creature(P0, "Other Bear", 2, 4)
            .with_subtypes(vec!["Bear"]);
        scenario.add_basic_land(P1, ManaColor::Green);
        scenario
            .add_creature(P1, "Foreign Bear", 2, 4)
            .with_subtypes(vec!["Bear"]);
        let victim = scenario
            .add_creature(victim_controller, "Fight target", 1, 1)
            .id();
        let bear = scenario
            .add_creature_to_hand_from_oracle(P0, "Drop Bear", 0, 0, DROP_BEAR)
            .with_subtypes(vec!["Koala", "Bear", "Horror"])
            .with_mana_cost(complete_publication_cost("{3}{R}{G}"))
            .id();
        let counter = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Battlegrowth",
                true,
                "Put a +1/+1 counter on target creature.",
            )
            .with_mana_cost(complete_publication_cost("{G}"))
            .id();
        let forest = scenario
            .add_land_to_hand(P0, "Forest")
            .with_subtypes(vec!["Forest"])
            .id();
        scenario.with_mana_pool(
            P0,
            complete_publication_pool(3, &[ManaType::Red, ManaType::Green]),
        );
        let mut runner = scenario.build();
        runner.cast(bear).commit();
        runner.pass_both_players();
        let WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("required fight target: {:?}", runner.state().waiting_for)
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, Some(bear));
        assert_eq!(target_slots.len(), 1);
        assert!(target_slots[selection.current_slot].optional);
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(victim)));
        runner
            .act(GameAction::ChooseTarget {
                target: fight.then_some(TargetRef::Object(victim)),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(effective_pt(&mut runner, bear), (5, 5));
        assert!(runner.state().objects[&bear]
            .keywords
            .contains(&Keyword::Flash));
        assert!(runner.state().objects[&bear]
            .keywords
            .contains(&Keyword::Haste));
        assert_eq!(
            runner.state().objects[&victim].zone,
            if fight {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_continuous_complete(runner.state());
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner
            .act(GameAction::PlayLand {
                object_id: forest,
                card_id: runner.state().objects[&forest].card_id,
            })
            .unwrap();
        assert_eq!(effective_pt(&mut runner, bear), (6, 6));
        complete_publication_fund(&mut runner, P0, 0, &[ManaType::Green]);
        runner.cast(counter).target_object(bear).resolve();
        // CR 604.3a + CR 613.4c: the live defining sum precedes counters.
        assert_eq!(effective_pt(&mut runner, bear), (7, 7));
        assert_eq!(runner.state().objects[&bear].layer_base_power, Some(6));
        assert_eq!(runner.state().objects[&bear].layer_base_toughness, Some(6));
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_rakdos_granted_cost_retains_granting_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knife = scenario
        .add_artifact_from_oracle(P0, "Rakdos Riteknife", RITEKNIFE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let other = scenario
        .add_artifact_from_oracle(P0, "Rakdos Riteknife", RITEKNIFE)
        .with_subtypes(vec!["Equipment"])
        .id();
    scenario.with_counter(other, CounterType::Generic("blood".into()), 5);
    let host = scenario.add_creature(P0, "Equipped host", 2, 3).id();
    let second_host = scenario.add_creature(P0, "Other host", 4, 5).id();
    let fodder = scenario.add_creature(P0, "Sacrifice choice", 1, 1).id();
    scenario.with_mana_pool(P0, complete_publication_pool(4, &[]));
    let mut runner = scenario.build();
    runner
        .act(GameAction::Equip {
            equipment_id: knife,
            target_id: host,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    runner
        .act(GameAction::Equip {
            equipment_id: other,
            target_id: second_host,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(effective_pt(&mut runner, second_host), (9, 5));
    let index = runner.state().objects[&host]
        .abilities
        .iter()
        .position(|definition| {
            definition
                .granting_object
                .is_some_and(|object| object.object_id == knife)
        })
        .expect("actual parsed granted ability");
    runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: host,
            ability_index: index,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("required sacrifice cost: {:?}", runner.state().waiting_for)
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert!(choices.contains(&fodder));
    assert!(choices.contains(&host));
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    // CR 301.5d: host pays tap/sacrifice; the by-name body counters its granter.
    assert!(runner.state().objects[&host].tapped);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&knife]
            .counters
            .get(&CounterType::Generic("blood".into())),
        Some(&1)
    );
    assert_eq!(
        runner.state().objects[&other]
            .counters
            .get(&CounterType::Generic("blood".into())),
        Some(&5)
    );
    assert!(!runner.state().objects[&host]
        .counters
        .contains_key(&CounterType::Generic("blood".into())));
    assert_eq!(effective_pt(&mut runner, host), (3, 3));
    assert_eq!(effective_pt(&mut runner, second_host), (9, 5));
    assert_continuous_complete(runner.state());
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: host,
                ability_index: index
            })
            .is_err(),
        "tapped host cannot pay again"
    );
}

#[test]
fn complete_continuous_elturel_counts_each_actual_player_and_planeswalker_defender() {
    use engine::game::combat::AttackTarget;
    use engine::types::game_state::CastPaymentMode;
    use engine::types::player::PlayerId;
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 1803);
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..2 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    for _ in 0..5 {
        scenario.add_basic_land(p2, ManaColor::Blue);
    }
    for _ in 0..7 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let walker = scenario
        .add_planeswalker_from_oracle(p2, "Defending walker", "Jace", 20, "")
        .id();
    let first = scenario
        .add_creature_from_oracle(P0, "Elturel Survivors", 0, 4, ELTUREL)
        .id();
    let second = scenario
        .add_creature_from_oracle(P0, "Elturel Survivors", 0, 4, ELTUREL)
        .id();
    let theft = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Turn Against",
            true,
            "Devoid (This card has no color.)\nGain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.",
        )
        .with_mana_cost(complete_publication_cost("{4}{R}"))
        .id();
    let mut runner = scenario.build();
    assert_eq!(effective_pt(&mut runner, first), (0, 4));
    assert_eq!(effective_pt(&mut runner, second), (0, 4));
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Planeswalker(walker)),
        ])
        .unwrap();
    for _ in 0..2 {
        runner.advance_until_stack_empty();
        let WaitingFor::OptionalEffectChoice {
            player, source_id, ..
        } = &runner.state().waiting_for
        else {
            panic!("required myriad option: {:?}", runner.state().waiting_for)
        };
        assert_eq!(*player, P0);
        assert!([first, second].contains(source_id));
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
    }
    runner.advance_until_stack_empty();
    // CR 508.1b: each source's actual defender, including the walker's controller.
    assert_eq!(effective_pt(&mut runner, first), (2, 4));
    assert_eq!(effective_pt(&mut runner, second), (5, 4));
    assert!(runner.state().objects[&first]
        .keywords
        .contains(&Keyword::Trample));
    assert_continuous_complete(runner.state());
    runner.advance_to_phase(Phase::DeclareBlockers);
    for expected in [P1, p2] {
        let WaitingFor::DeclareBlockers { player, .. } = runner.state().waiting_for else {
            panic!(
                "required defender blockers: {:?}",
                runner.state().waiting_for
            )
        };
        assert_eq!(player, expected);
        runner.declare_blockers(&[]).unwrap();
    }
    let epoch = runner.state().objects[&first].incarnation;
    complete_publication_fund(&mut runner, P1, 4, &[ManaType::Red]);
    assert_eq!(runner.state().priority_player, P0);
    runner.act(GameAction::PassPriority).unwrap();
    runner
        .act(GameAction::CastSpell {
            object_id: theft,
            card_id: runner.state().objects[&theft].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::default(),
        })
        .unwrap();
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("required control target: {:?}", runner.state().waiting_for)
    };
    assert_eq!(*player, P1);
    assert_eq!(target_slots.len(), 1);
    assert_eq!(selection.current_slot, 0);
    let slot = &target_slots[selection.current_slot];
    assert!(!slot.optional);
    assert!(slot.legal_targets.contains(&TargetRef::Object(first)));
    assert!(slot.legal_targets.contains(&TargetRef::Object(second)));
    assert!(!slot.legal_targets.contains(&TargetRef::Object(walker)));
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(walker)),
        })
        .is_err());
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(first)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[1].mana_pool.total(), 0);
    assert_eq!(runner.state().objects[&theft].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&first].owner, P0);
    assert_eq!(runner.state().objects[&first].controller, P1);
    assert_eq!(runner.state().objects[&first].incarnation, epoch);
    assert!(!runner.state().objects[&first].tapped);
    assert!(runner.state().objects[&first]
        .keywords
        .contains(&Keyword::Haste));
    // CR 506.4: a controller change removes this source from combat, ending
    // its live attacking bonus without changing its owner or incarnation.
    let combat = runner.state().combat.as_ref().unwrap();
    assert!(!combat
        .attackers
        .iter()
        .any(|attacker| attacker.object_id == first));
    let still_attacking = combat
        .attackers
        .iter()
        .find(|attacker| attacker.object_id == second)
        .unwrap();
    assert_eq!(
        still_attacking.attack_target,
        AttackTarget::Planeswalker(walker)
    );
    assert_eq!(still_attacking.defending_player, p2);
    assert_eq!(effective_pt(&mut runner, first), (0, 4));
    assert_eq!(effective_pt(&mut runner, second), (5, 4));
    assert_continuous_complete(runner.state());
    runner.advance_to_phase(Phase::PostCombatMain);
    assert_eq!(effective_pt(&mut runner, first), (0, 4));
    assert_eq!(effective_pt(&mut runner, second), (0, 4));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_source_damage_history_drives_real_target_legality_and_new_incarnation() {
    use engine::game::combat::AttackTarget;
    use engine::types::game_state::CastPaymentMode;
    for prevented in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(P0, "Oyaminartok, Polar Werebear", 4, 5, OYAMINARTOK)
            .as_legendary()
            .with_mana_cost(complete_publication_cost("{2}{G}{G}"))
            .id();
        let untouched = scenario
            .add_creature_from_oracle(P1, "Oyaminartok, Polar Werebear", 4, 5, OYAMINARTOK)
            .as_legendary()
            .id();
        let decoy = scenario
            .add_creature(P0, "Unprotected legal control", 2, 2)
            .id();
        let initial_bounce = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{U}"))
            .id();
        let later_bounce = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{U}"))
            .id();
        let fog = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Fog",
                true,
                "Prevent all combat damage that would be dealt this turn.",
            )
            .with_mana_cost(complete_publication_cost("{G}"))
            .id();
        let mut runner = scenario.build();
        effective_pt(&mut runner, source);
        assert!(runner.state().objects[&source]
            .keywords
            .contains(&Keyword::Hexproof));
        complete_publication_fund(&mut runner, P1, 0, &[ManaType::Blue]);
        runner.act(GameAction::PassPriority).unwrap();
        runner
            .act(GameAction::CastSpell {
                object_id: initial_bounce,
                card_id: runner.state().objects[&initial_bounce].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::default(),
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("required target prompt: {:?}", runner.state().waiting_for)
        };
        assert_eq!(*player, P1);
        assert!(!target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(source)));
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(decoy)));
        assert!(runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(source))
            })
            .is_err());
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(decoy)),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&decoy].zone, Zone::Hand);
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(source, AttackTarget::Player(P1))])
            .unwrap();
        if prevented {
            complete_publication_fund(&mut runner, P0, 0, &[ManaType::Green]);
            runner.cast(fog).resolve();
        }
        runner.advance_to_phase(Phase::DeclareBlockers);
        let WaitingFor::DeclareBlockers { player, .. } = runner.state().waiting_for else {
            panic!("blockers")
        };
        assert_eq!(player, P1);
        runner.declare_blockers(&[]).unwrap();
        runner.advance_to_phase(Phase::PostCombatMain);
        effective_pt(&mut runner, source);
        // CR 120.3: prevention produces no dealt-damage history; the other source never dealt damage.
        assert_eq!(
            runner.state().objects[&source]
                .keywords
                .contains(&Keyword::Hexproof),
            prevented
        );
        assert!(runner.state().objects[&untouched]
            .keywords
            .contains(&Keyword::Hexproof));
        let food = runner
            .state()
            .objects
            .values()
            .filter(|object| {
                object.zone == Zone::Battlefield
                    && object.is_token
                    && object
                        .card_types
                        .subtypes
                        .iter()
                        .any(|subtype| subtype == "Food")
            })
            .count();
        assert_eq!(food, usize::from(!prevented));
        assert_continuous_complete(runner.state());
        if !prevented {
            complete_publication_fund(&mut runner, P1, 0, &[ManaType::Blue]);
            runner.act(GameAction::PassPriority).unwrap();
            runner
                .act(GameAction::CastSpell {
                    object_id: later_bounce,
                    card_id: runner.state().objects[&later_bounce].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::default(),
                })
                .unwrap();
            let WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("later target prompt")
            };
            assert!(target_slots[selection.current_slot]
                .legal_targets
                .contains(&TargetRef::Object(source)));
            assert!(target_slots[selection.current_slot]
                .legal_targets
                .contains(&TargetRef::Object(untouched)));
            let incarnation = runner.state().objects[&source].incarnation;
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(source)),
                })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
            for mana in complete_publication_pool(2, &[ManaType::Green, ManaType::Green]) {
                runner.state_mut().add_mana_to_pool(P0, mana).unwrap();
            }
            runner.cast(source).resolve();
            effective_pt(&mut runner, source);
            // CR 400.7: the returned incarnation starts with no old damage history.
            assert_ne!(runner.state().objects[&source].incarnation, incarnation);
            assert!(runner.state().objects[&source]
                .keywords
                .contains(&Keyword::Hexproof));
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn complete_continuous_mimics_retain_both_local_durations_from_real_dual_color_casts() {
    for (name, cost, text, spell_name, spell_cost, spell_text, spell_types, first_color, second_color, power, toughness, keyword) in [
        ("Battlegate Mimic", "{1}{R/W}", "Whenever you cast a spell that's both red and white, this creature has base power and toughness 4/2 until end of turn and gains first strike until end of turn.", "Boros Recruit", "{R/W}", "({R/W} can be paid with either {R} or {W}.)\nFirst strike", vec!["Goblin","Soldier"], ManaColor::Red, ManaColor::White, 4, 2, Keyword::FirstStrike),
        ("Nightsky Mimic", "{1}{W/B}", "Whenever you cast a spell that's both white and black, this creature has base power and toughness 4/4 until end of turn and gains flying until end of turn.", "Nip Gwyllion", "{W/B}", "Lifelink (Damage dealt by this creature also causes you to gain that much life.)", vec!["Hag"], ManaColor::White, ManaColor::Black, 4, 4, Keyword::Flying),
        ("Shorecrasher Mimic", "{1}{G/U}", "Whenever you cast a spell that's both green and blue, this creature has base power and toughness 5/3 until end of turn and gains trample until end of turn.", "Slippery Bogle", "{G/U}", "Hexproof (This creature can't be the target of spells or abilities your opponents control.)", vec!["Beast"], ManaColor::Green, ManaColor::Blue, 5, 3, Keyword::Trample),
        ("Woodlurker Mimic", "{1}{B/G}", "Whenever you cast a spell that's both black and green, this creature has base power and toughness 4/5 until end of turn and gains wither until end of turn. (It deals damage to creatures in the form of -1/-1 counters.)", "Odious Trow", "{B/G}", "{1}{B/G}: Regenerate this creature.", vec!["Troll"], ManaColor::Black, ManaColor::Green, 4, 5, Keyword::Wither),
    ] {
        let mut scenario = GameScenario::new(); scenario.at_phase(Phase::PreCombatMain);
        let mimic = scenario.add_creature_from_oracle(P0, name, 2, 1, text).with_subtypes(vec!["Shapeshifter"])
            .with_mana_cost(complete_publication_cost(cost)).with_plus_counters(1).id();
        let foreign = scenario.add_creature_from_oracle(P1, name, 2, 1, text).with_subtypes(vec!["Shapeshifter"])
            .with_mana_cost(complete_publication_cost(cost)).id();
        let spell = scenario.add_creature_to_hand_from_oracle(P0, spell_name, 1, 1, spell_text)
            .with_subtypes(spell_types).with_color(vec![first_color, second_color]).with_mana_cost(complete_publication_cost(spell_cost)).id();
        let monocolor = scenario.add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
            .with_subtypes(vec!["Bear"]).with_color(vec![ManaColor::Green]).with_mana_cost(complete_publication_cost("{1}{G}")).id();
        for player in [P0, P1] { for _ in 0..4 { scenario.add_card_to_library_top(player, "Turn buffer"); } }
        let mut runner = scenario.build();
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Green]);
        runner.cast(monocolor).resolve();
        assert_eq!(effective_pt(&mut runner, mimic), (3, 2));
        assert!(!runner.state().objects[&mimic].keywords.contains(&keyword));
        assert_continuous_complete(runner.state());
        let mana = match first_color { ManaColor::White => ManaType::White, ManaColor::Blue => ManaType::Blue,
            ManaColor::Black => ManaType::Black, ManaColor::Red => ManaType::Red, ManaColor::Green => ManaType::Green };
        complete_publication_fund(&mut runner, P0, 0, &[mana]);
        runner.cast(spell).commit();
        assert_eq!(runner.state().stack.len(), 2, "cast spell plus actual Mimic trigger: {name}");
        assert_eq!(runner.state().stack.last().unwrap().ability().unwrap().source_id, mimic);
        runner.advance_until_stack_empty();
        assert_eq!(effective_pt(&mut runner, mimic), (power + 1, toughness + 1));
        assert!(runner.state().objects[&mimic].keywords.contains(&keyword));
        assert_eq!(effective_pt(&mut runner, foreign), (2, 1));
        assert!(!runner.state().objects[&foreign].keywords.contains(&keyword));
        assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
        assert_continuous_complete(runner.state());
        runner.advance_to_combat(); runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step(); runner.pass_both_players();
        assert_eq!(effective_pt(&mut runner, mimic), (3, 2));
        assert!(!runner.state().objects[&mimic].keywords.contains(&keyword));
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_aven_feather_duration_survives_source_and_ends_on_exact_recipient() {
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        let source = scenario.add_creature_from_oracle(P0, "Aven Mimeomancer", 3, 1,
            "Flying\nAt the beginning of your upkeep, you may put a feather counter on target creature. If you do, that creature has base power and toughness 3/1 and has flying for as long as it has a feather counter on it.")
            .with_subtypes(vec!["Bird", "Wizard"]).with_mana_cost(complete_publication_cost("{1}{W}{U}")).id();
        let recipient = scenario
            .add_creature(P0, "Chosen unequal creature", 2, 7)
            .id();
        let other = scenario.add_creature(P1, "Counter receiver", 6, 8).id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Disperse",
                true,
                "Return target nonland permanent to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{1}{U}"))
            .id();
        let transfer = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Fate Transfer",
                true,
                "Move all counters from target creature onto another target creature.",
            )
            .with_mana_cost(complete_publication_cost("{1}{U/B}"))
            .id();
        let mut runner = scenario.build();
        runner.advance_to_upkeep();
        let WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual upkeep target: {:?}", runner.state().waiting_for);
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, Some(source));
        assert_eq!(target_slots.len(), 1);
        assert!(!target_slots[selection.current_slot].optional);
        for object in [source, recipient, other] {
            assert!(target_slots[selection.current_slot]
                .legal_targets
                .contains(&TargetRef::Object(object)));
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(recipient)),
            })
            .unwrap();
        runner.resolve_top();
        let WaitingFor::OptionalEffectChoice {
            player, source_id, ..
        } = &runner.state().waiting_for
        else {
            panic!("required feather optional decision");
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, source);
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(
            effective_pt(&mut runner, recipient),
            if accept { (3, 1) } else { (2, 7) }
        );
        assert_eq!(
            runner.state().objects[&recipient]
                .keywords
                .contains(&Keyword::Flying),
            accept
        );
        assert_eq!(
            runner.state().objects[&recipient]
                .counters
                .get(&CounterType::Generic("feather".into()))
                .copied()
                .unwrap_or(0),
            u32::from(accept)
        );
        assert_eq!(effective_pt(&mut runner, other), (6, 8));
        assert_continuous_complete(runner.state());
        if accept {
            complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
            runner.cast(bounce).target_object(source).resolve();
            assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
            assert_eq!(effective_pt(&mut runner, recipient), (3, 1));
            assert!(runner.state().objects[&recipient]
                .keywords
                .contains(&Keyword::Flying));
            assert_continuous_complete(runner.state());
            complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
            runner
                .cast(transfer)
                .target_objects(&[recipient, other])
                .resolve();
            assert_eq!(effective_pt(&mut runner, recipient), (2, 7));
            assert!(!runner.state().objects[&recipient]
                .keywords
                .contains(&Keyword::Flying));
            assert_eq!(
                runner.state().objects[&other]
                    .counters
                    .get(&CounterType::Generic("feather".into())),
                Some(&1)
            );
            assert_eq!(effective_pt(&mut runner, other), (6, 8));
            assert!(!runner.state().objects[&other]
                .keywords
                .contains(&Keyword::Flying));
            assert_continuous_complete(runner.state());
        }
    }
}

// CR 608.2d + CR 611.2c: each real ETB chooses for its own controller and
// fixes the matching recipients when it resolves, independently of later choices.
#[test]
fn complete_continuous_safewright_two_public_type_choices_keep_exact_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Safewright", 4, 2, SAFEWRIGHT)
        .with_subtypes(vec!["Elf", "Warrior"])
        .with_color(vec![ManaColor::Green])
        .with_mana_cost(complete_publication_cost("{3}{G}{G}"))
        .id();
    let second = scenario
        .add_creature_to_hand_from_oracle(P1, "Selfless Safewright", 4, 2, SAFEWRIGHT)
        .with_subtypes(vec!["Elf", "Warrior"])
        .with_color(vec![ManaColor::Green])
        .with_mana_cost(complete_publication_cost("{3}{G}{G}"))
        .id();
    let own_elf = scenario
        .add_creature(P0, "Own Elf", 2, 3)
        .with_subtypes(vec!["Elf"])
        .id();
    let own_goblin = scenario
        .add_creature(P0, "Own Goblin", 3, 4)
        .with_subtypes(vec!["Goblin"])
        .id();
    let foreign_elf = scenario
        .add_creature(P1, "Foreign Elf", 4, 5)
        .with_subtypes(vec!["Elf"])
        .id();
    let foreign_goblin = scenario
        .add_creature(P1, "Foreign Goblin", 5, 6)
        .with_subtypes(vec!["Goblin"])
        .id();
    let kindred = scenario.add_enchantment_from_oracle(P0, "Prowess of the Fair",
        "Whenever another nontoken Elf is put into your graveyard from the battlefield, you may create a 1/1 green Elf Warrior creature token.")
        .with_subtypes(vec!["Elf"]).with_color(vec![ManaColor::Black]).with_mana_cost(complete_publication_cost("{1}{B}")).id();
    let foreign_kindred = scenario.add_enchantment_from_oracle(P1, "Prowess of the Fair",
        "Whenever another nontoken Elf is put into your graveyard from the battlefield, you may create a 1/1 green Elf Warrior creature token.")
        .with_subtypes(vec!["Elf"]).with_color(vec![ManaColor::Black]).with_mana_cost(complete_publication_cost("{1}{B}")).id();
    let late = scenario
        .add_creature_to_hand_from_oracle(P0, "Llanowar Elves", 1, 1, "{T}: Add {G}.")
        .with_subtypes(vec!["Elf", "Druid"])
        .with_color(vec![ManaColor::Green])
        .with_mana_cost(complete_publication_cost("{G}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    for player in [P0, P1] {
        for _ in 0..4 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    for object in [kindred, foreign_kindred] {
        let printed = runner.state_mut().objects.get_mut(&object).unwrap();
        printed
            .card_types
            .core_types
            .push(engine::types::card_type::CoreType::Kindred);
        printed.base_card_types = printed.card_types.clone();
        assert!(!printed
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature));
    }
    for (source, player, answer) in [(first, P0, "Elf"), (second, P1, "Goblin")] {
        if player == P1 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        complete_publication_fund(&mut runner, player, 3, &[ManaType::Green, ManaType::Green]);
        runner.cast(source).resolve();
        let WaitingFor::NamedChoice {
            player: actor,
            choice_type,
            options,
            source: authority,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "actual paid Safewright ETB choice: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*actor, player);
        assert!(matches!(
            choice_type,
            engine::types::ability::ChoiceType::CreatureType { .. }
        ));
        assert!(options.len() > 20);
        assert!(options.iter().any(|option| option == "Elf"));
        assert!(options.iter().any(|option| option == "Goblin"));
        assert_ne!(options.first().map(String::as_str), Some(answer));
        assert_eq!(
            authority
                .as_ref()
                .unwrap()
                .prompt
                .identity
                .reference
                .object_id,
            source
        );
        runner
            .act(GameAction::ChooseOption {
                choice: answer.into(),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().players[player.0 as usize].mana_pool.total(),
            0
        );
        effective_pt(&mut runner, own_elf);
        for (object, expected) in [
            (own_elf, true),
            (own_goblin, false),
            (foreign_elf, false),
            (foreign_goblin, player == P1),
            (kindred, true),
            (foreign_kindred, false),
            (first, false),
            (source, false),
        ] {
            for keyword in [Keyword::Hexproof, Keyword::Indestructible] {
                assert_eq!(
                    runner.state().objects[&object].keywords.contains(&keyword),
                    expected,
                    "{answer}: {object:?} {keyword:?}"
                );
            }
        }
        assert_continuous_complete(runner.state());
    }
    // The second controller's answer cannot replace the first resolved answer.
    if runner.state().priority_player != P0 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Green, ManaType::Blue]);
    runner.cast(late).resolve();
    for keyword in [Keyword::Hexproof, Keyword::Indestructible] {
        assert!(!runner.state().objects[&late].keywords.contains(&keyword));
        assert!(runner.state().objects[&own_elf].keywords.contains(&keyword));
        assert!(runner.state().objects[&kindred].keywords.contains(&keyword));
        assert!(runner.state().objects[&foreign_goblin]
            .keywords
            .contains(&keyword));
    }
    runner.cast(bounce).target_object(first).resolve();
    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
    for keyword in [Keyword::Hexproof, Keyword::Indestructible] {
        assert!(runner.state().objects[&own_elf].keywords.contains(&keyword));
        assert!(runner.state().objects[&foreign_goblin]
            .keywords
            .contains(&keyword));
    }
    assert_continuous_complete(runner.state());
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    for object in [
        own_elf,
        own_goblin,
        foreign_elf,
        foreign_goblin,
        kindred,
        foreign_kindred,
        late,
        second,
    ] {
        for keyword in [Keyword::Hexproof, Keyword::Indestructible] {
            assert!(!runner.state().objects[&object].keywords.contains(&keyword));
        }
    }
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_entity_x_zero_two_and_lifetime_hostiles() {
    for x in [0, 2] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(P0, "Mirror Entity", 1, 1, ENTITY)
            .with_subtypes(vec!["Shapeshifter"])
            .with_mana_cost(complete_publication_cost("{2}{W}"))
            .id();
        let plain = scenario
            .add_creature(P0, "Plain recipient", 2, 3)
            .with_subtypes(vec!["Bear"])
            .id();
        let countered = scenario
            .add_creature(P0, "Counter recipient", 2, 3)
            .with_plus_counters(1)
            .id();
        let foreign = scenario.add_creature(P1, "Foreign recipient", 6, 7).id();
        let late = scenario
            .add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(complete_publication_cost("{1}{G}"))
            .id();
        let growth = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Giant Growth",
                true,
                "Target creature gets +3/+3 until end of turn.",
            )
            .with_mana_cost(complete_publication_cost("{G}"))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Disperse",
                true,
                "Return target nonland permanent to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{1}{U}"))
            .id();
        let blink = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true,
            "Exile target creature you control, then return that card to the battlefield under your control.")
            .with_mana_cost(complete_publication_cost("{W}")).id();
        for player in [P0, P1] {
            for _ in 0..4 {
                scenario.add_card_to_library_top(player, "Draw buffer");
            }
        }
        scenario.with_mana_pool(P0, complete_publication_pool(x as usize, &[]));
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        let WaitingFor::ChooseXValue {
            player, min, max, ..
        } = &runner.state().waiting_for
        else {
            panic!("announced X prompt")
        };
        assert_eq!(*player, P0);
        assert_eq!(*min, 0);
        assert_eq!(*max, x);
        assert!(
            runner.act(GameAction::ChooseX { value: 4 }).is_err(),
            "unfunded announced X"
        );
        runner.act(GameAction::ChooseX { value: x }).unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        // CR 704.5f: X=0 actually kills unmodified recipients; +1 counters
        // remain in layer 7c and save the countered recipient at 1/1.
        if x == 0 {
            assert_eq!(runner.state().objects[&plain].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            assert_eq!(effective_pt(&mut runner, countered), (1, 1));
        } else {
            assert_eq!(effective_pt(&mut runner, plain), (2, 2));
            assert_eq!(effective_pt(&mut runner, source), (2, 2));
            assert_eq!(effective_pt(&mut runner, countered), (3, 3));
        }
        assert!(runner.state().objects[&countered]
            .keywords
            .contains(&Keyword::Changeling));
        assert!(runner.state().objects[&countered].card_types.subtypes.len() > 20);
        assert_eq!(effective_pt(&mut runner, foreign), (6, 7));
        assert!(!runner.state().objects[&foreign]
            .keywords
            .contains(&Keyword::Changeling));
        assert_continuous_complete(runner.state());
        complete_publication_fund(
            &mut runner,
            P0,
            2,
            &[
                ManaType::Green,
                ManaType::Green,
                ManaType::Blue,
                ManaType::White,
            ],
        );
        runner.cast(growth).target_object(countered).resolve();
        assert_eq!(
            effective_pt(&mut runner, countered),
            (x as i32 + 4, x as i32 + 4)
        );
        runner.cast(late).resolve();
        assert_eq!(effective_pt(&mut runner, late), (2, 2));
        assert!(!runner.state().objects[&late]
            .keywords
            .contains(&Keyword::Changeling));
        if x == 2 {
            runner.cast(bounce).target_object(source).resolve();
            assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
            assert_eq!(effective_pt(&mut runner, plain), (2, 2));
            assert!(runner.state().objects[&plain]
                .keywords
                .contains(&Keyword::Changeling));
            let before = runner.state().objects[&plain].incarnation;
            runner.cast(blink).target_object(plain).resolve();
            assert_ne!(runner.state().objects[&plain].incarnation, before);
            assert_eq!(effective_pt(&mut runner, plain), (2, 3));
            assert!(!runner.state().objects[&plain]
                .keywords
                .contains(&Keyword::Changeling));
        }
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        assert_eq!(effective_pt(&mut runner, countered), (3, 4));
        assert!(!runner.state().objects[&countered]
            .keywords
            .contains(&Keyword::Changeling));
        assert_continuous_complete(runner.state());
    }
}
#[test]
fn complete_continuous_paid_aura_classes_keep_companions_control_and_source_lifetime() {
    for (name, cost, text, generic, colors, expected, keywords, assassin, steals) in [
        ("Coerced to Kill", "{3}{U}{B}", "Enchant creature\nYou control enchanted creature.\nEnchanted creature has base power and toughness 1/1, has deathtouch, and is an Assassin in addition to its other types.", 3, vec![ManaType::Blue, ManaType::Black], (1,1), vec![Keyword::Deathtouch], true, true),
        ("Gigantiform", "{3}{G}{G}", "Kicker {4}\nEnchant creature\nEnchanted creature has base power and toughness 8/8 and has trample.\nWhen this Aura enters, if it was kicked, you may search your library for a card named Gigantiform, put it onto the battlefield, then shuffle.", 3, vec![ManaType::Green, ManaType::Green], (8,8), vec![Keyword::Trample], false, false),
        ("Super State", "{7}", "Enchant creature you control\nEnchanted creature has base power and toughness 9/9 and has flying, first strike, trample, and haste.\nWhenever enchanted creature deals combat damage to an opponent, it deals that much damage to each other opponent.", 7, vec![], (9,9), vec![Keyword::Flying, Keyword::FirstStrike, Keyword::Trample, Keyword::Haste], false, false),
    ] {
        let mut scenario = GameScenario::new(); scenario.at_phase(Phase::PreCombatMain);
        let host_owner = if steals { P1 } else { P0 };
        let host = scenario.add_creature(host_owner, "Unequal Goblin host", 2, 3).with_subtypes(vec!["Goblin"]).with_plus_counters(1).id();
        let decoy = scenario.add_creature(P1, "Foreign unenchanted", 6, 7).id();
        let mut aura_builder = scenario.add_spell_to_hand_from_oracle(P0, name, false, text);
        aura_builder.as_enchantment().with_subtypes(vec!["Aura"]).with_mana_cost(complete_publication_cost(cost));
        if name == "Super State" { aura_builder.as_legendary(); }
        let source = aura_builder.id();
        let bounce = scenario.add_spell_to_hand_from_oracle(P0, "Disperse", true,
            "Return target nonland permanent to its owner's hand.").with_mana_cost(complete_publication_cost("{1}{U}")).id();
        let mut runner = scenario.build();
        assert!(runner.cast(source).target_object(host).try_resolve().is_err(), "unfunded full cast {name}");
        complete_publication_fund(&mut runner, P0, generic, &colors);
        runner.cast(source).target_object(host).resolve();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(effective_pt(&mut runner, host), (expected.0+1, expected.1+1));
        assert_eq!(runner.state().objects[&host].owner, host_owner);
        assert_eq!(runner.state().objects[&host].controller, P0);
        for keyword in &keywords { assert!(runner.state().objects[&host].keywords.contains(keyword)); assert!(!runner.state().objects[&decoy].keywords.contains(keyword)); }
        assert!(runner.state().objects[&host].card_types.subtypes.iter().any(|subtype| subtype == "Goblin"));
        assert_eq!(runner.state().objects[&host].card_types.subtypes.iter().any(|subtype| subtype == "Assassin"), assassin);
        assert_eq!(effective_pt(&mut runner, decoy), (6,7));
        assert_continuous_complete(runner.state());
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
        runner.cast(bounce).target_object(source).resolve();
        assert_eq!(effective_pt(&mut runner, host), (3,4));
        assert_eq!(runner.state().objects[&host].controller, host_owner);
        for keyword in &keywords { assert!(!runner.state().objects[&host].keywords.contains(keyword)); }
        assert!(!runner.state().objects[&host].card_types.subtypes.iter().any(|subtype| subtype == "Assassin"));
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_hulkbuster_and_bestowed_hydra_use_paid_attachments() {
    use engine::types::actions::AlternativeCastDecision;
    for hydra in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Unequal host", 2, 3).id();
        let decoy = scenario.add_creature(P0, "Second host", 4, 6).id();
        let foreign = scenario.add_creature(P1, "Foreign host", 7, 8).id();
        let source = if hydra {
            scenario.add_creature_to_hand_from_oracle(P0, "Nyxborn Hydra", 0, 1,
                "Bestow {X}{G}{G} (If you cast this card for its bestow cost, it's an Aura spell with enchant creature. It becomes a creature again if it's not attached.)\nReach, trample\nThis permanent enters with X +1/+1 counters on it.\nEnchanted creature gets +1/+1 for each +1/+1 counter on this Aura and has reach and trample.")
                .as_enchantment().as_creature().with_subtypes(vec!["Hydra"]).with_mana_cost(complete_publication_cost("{X}{G}")).id()
        } else {
            scenario.add_artifact_to_hand_from_oracle(P0, "Hulkbuster Armor",
                "Equipped creature has base power and toughness 9/9 and has flying.\nEquip Hero {3}\nEquip {6}")
                .as_legendary().with_subtypes(vec!["Equipment"]).with_mana_cost(complete_publication_cost("{4}")).id()
        };
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Disperse",
                true,
                "Return target nonland permanent to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{1}{U}"))
            .id();
        let mut runner = scenario.build();
        if hydra {
            complete_publication_fund(&mut runner, P0, 2, &[ManaType::Green, ManaType::Green]);
            runner
                .cast(source)
                .alternative_cast(AlternativeCastDecision::Alternative)
                .x(2)
                .target_object(host)
                .resolve();
            assert_eq!(
                runner.state().objects[&source]
                    .counters
                    .get(&CounterType::Plus1Plus1),
                Some(&2)
            );
            assert_eq!(effective_pt(&mut runner, host), (4, 5));
            for keyword in [Keyword::Reach, Keyword::Trample] {
                assert!(runner.state().objects[&host].keywords.contains(&keyword));
            }
        } else {
            complete_publication_fund(&mut runner, P0, 4, &[]);
            runner.cast(source).resolve();
            assert!(runner
                .act(GameAction::Equip {
                    equipment_id: source,
                    target_id: host
                })
                .is_err());
            complete_publication_fund(&mut runner, P0, 6, &[]);
            runner
                .act(GameAction::Equip {
                    equipment_id: source,
                    target_id: host,
                })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_eq!(effective_pt(&mut runner, host), (9, 9));
            assert!(runner.state().objects[&host]
                .keywords
                .contains(&Keyword::Flying));
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(effective_pt(&mut runner, decoy), (4, 6));
        assert_eq!(effective_pt(&mut runner, foreign), (7, 8));
        assert_continuous_complete(runner.state());
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
        runner.cast(bounce).target_object(source).resolve();
        assert_eq!(effective_pt(&mut runner, host), (2, 3));
        for keyword in [Keyword::Flying, Keyword::Reach, Keyword::Trample] {
            assert!(!runner.state().objects[&host].keywords.contains(&keyword));
        }
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_rakdos_self_sacrifice_keeps_granter_and_printed_br_body() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knife = scenario
        .add_artifact_from_oracle(P0, "Rakdos Riteknife", RITEKNIFE)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(complete_publication_cost("{2}"))
        .id();
    let host = scenario
        .add_creature(P0, "Sole controlled creature", 2, 3)
        .id();
    let first = scenario
        .add_creature(P1, "First sacrifice option", 3, 4)
        .id();
    let second = scenario
        .add_creature(P1, "Nondefault sacrifice option", 5, 6)
        .id();
    scenario.with_mana_pool(P0, complete_publication_pool(2, &[]));
    let mut runner = scenario.build();
    runner
        .act(GameAction::Equip {
            equipment_id: knife,
            target_id: host,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    let index = runner.state().objects[&host]
        .abilities
        .iter()
        .position(|definition| {
            definition
                .granting_object
                .is_some_and(|reference| reference.object_id == knife)
        })
        .unwrap();
    runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: host,
            ability_index: index,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("sole-host sacrifice cost")
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(choices, &vec![host]);
    runner
        .act(GameAction::SelectCards { cards: vec![host] })
        .unwrap();
    runner.advance_until_stack_empty();
    // CR 201.5a + CR 301.5d: the host may pay by sacrificing itself while
    // the granting Equipment remains to receive its specifically named counter.
    assert_eq!(runner.state().objects[&host].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&knife].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&knife]
            .counters
            .get(&CounterType::Generic("blood".into())),
        Some(&1)
    );
    assert_continuous_complete(runner.state());
    assert!(runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: host,
            ability_index: index
        })
        .is_err());
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: knife,
                ability_index: 0
            })
            .is_err(),
        "unfunded printed BR ability"
    );
    complete_publication_fund(&mut runner, P0, 0, &[ManaType::Blue, ManaType::Blue]);
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: knife,
                ability_index: 0
            })
            .is_err(),
        "wrong colors cannot pay BR"
    );
    complete_publication_fund(&mut runner, P0, 0, &[ManaType::Black, ManaType::Red]);
    runner.activate(knife, 0).target_player(P1).resolve();
    assert_eq!(
        runner.state().objects[&knife].zone,
        Zone::Graveyard,
        "real source sacrifice paid before body"
    );
    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        min_count,
        up_to,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("printed targeted-player sacrifice")
    };
    assert_eq!(*player, P1);
    assert_eq!(*count, 1);
    assert_eq!(*min_count, 1);
    assert!(!up_to);
    assert_eq!(cards.len(), 2);
    assert!(cards.contains(&first));
    assert!(cards.contains(&second));
    runner
        .act(GameAction::SelectCards {
            cards: vec![second],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_taranika_actual_attack_untaps_and_preserves_indestructible() {
    use engine::game::combat::AttackTarget;
    for attacking_recipient in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_creature_from_oracle(P0, "Taranika, Akroan Veteran", 3, 3,
            "Vigilance\nWhenever Taranika attacks, untap another target creature you control. Until end of turn, that creature has base power and toughness 4/4 and gains indestructible.")
            .as_legendary().with_subtypes(vec!["Human", "Soldier"]).with_color(vec![ManaColor::White])
            .with_mana_cost(complete_publication_cost("{1}{W}{W}")).id();
        let recipient = scenario
            .add_creature(P0, "Unequal countered recipient", 2, 7)
            .with_plus_counters(1)
            .id();
        let other = scenario.add_creature(P0, "Other legal creature", 6, 8).id();
        let foreign = scenario
            .add_creature(P1, "Foreign illegal creature", 4, 6)
            .id();
        for player in [P0, P1] {
            for _ in 0..4 {
                scenario.add_card_to_library_top(player, "Draw buffer");
            }
        }
        let mut runner = scenario.build();
        runner.advance_to_combat();
        let mut attackers = vec![(source, AttackTarget::Player(P1))];
        if attacking_recipient {
            attackers.push((recipient, AttackTarget::Player(P1)));
        }
        runner.declare_attackers(&attackers).unwrap();
        let WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual attack target")
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, Some(source));
        assert_eq!(target_slots.len(), 1);
        let slot = &target_slots[selection.current_slot];
        assert!(!slot.optional);
        assert!(slot.legal_targets.contains(&TargetRef::Object(recipient)));
        assert!(slot.legal_targets.contains(&TargetRef::Object(other)));
        assert!(!slot.legal_targets.contains(&TargetRef::Object(source)));
        assert!(!slot.legal_targets.contains(&TargetRef::Object(foreign)));
        assert_eq!(
            runner.state().objects[&recipient].tapped,
            attacking_recipient
        );
        assert!(runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(foreign))
            })
            .is_err());
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(recipient)),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert!(!runner.state().objects[&recipient].tapped);
        assert_eq!(effective_pt(&mut runner, recipient), (5, 5));
        assert!(runner.state().objects[&recipient]
            .keywords
            .contains(&Keyword::Indestructible));
        assert_eq!(effective_pt(&mut runner, other), (6, 8));
        assert!(!runner.state().objects[&other]
            .keywords
            .contains(&Keyword::Indestructible));
        assert_eq!(effective_pt(&mut runner, foreign), (4, 6));
        assert_eq!(effective_pt(&mut runner, source), (3, 3));
        assert!(!runner.state().objects[&source].tapped);
        assert!(runner.state().objects[&source]
            .keywords
            .contains(&Keyword::Vigilance));
        assert_continuous_complete(runner.state());
        runner.advance_to_phase(Phase::DeclareBlockers);
        runner.declare_blockers(&[]).unwrap();
        runner.advance_to_phase(Phase::PostCombatMain);
        assert_continuous_complete(runner.state());
        runner.advance_to_end_step();
        runner.pass_both_players();
        assert_eq!(effective_pt(&mut runner, recipient), (3, 8));
        assert!(!runner.state().objects[&recipient]
            .keywords
            .contains(&Keyword::Indestructible));
        assert_continuous_complete(runner.state());
    }
}
#[test]
fn complete_continuous_equipment_paid_entry_and_real_counter_producers() {
    for scythe in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let name = if scythe {
            "Reaper's Scythe"
        } else {
            "Mace of the Valiant"
        };
        let source = scenario
            .add_artifact_to_hand_from_oracle(P0, name, if scythe { SCYTHE } else { MACE })
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(complete_publication_cost(if scythe {
                "{2}{B}"
            } else {
                "{2}{W}"
            }))
            .id();
        let host = scenario
            .add_creature(P0, "Unequal equipped creature", 2, 3)
            .id();
        let foreign = scenario
            .add_creature(P1, "Unaffected foreign creature", 4, 6)
            .id();
        let entrant = scenario
            .add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(complete_publication_cost("{1}{G}"))
            .id();
        let bolt = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Lightning Bolt",
                true,
                "Lightning Bolt deals 3 damage to any target.",
            )
            .with_mana_cost(complete_publication_cost("{R}"))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Disperse",
                true,
                "Return target nonland permanent to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{1}{U}"))
            .id();
        let mut runner = scenario.build();
        complete_publication_fund(
            &mut runner,
            P0,
            2,
            &[if scythe {
                ManaType::Black
            } else {
                ManaType::White
            }],
        );
        runner.cast(source).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        if scythe {
            let heroes = runner
                .state()
                .battlefield
                .iter()
                .copied()
                .filter(|object| {
                    let object = &runner.state().objects[object];
                    object.is_token
                        && object.controller == P0
                        && object
                            .card_types
                            .subtypes
                            .iter()
                            .any(|subtype| subtype == "Hero")
                })
                .collect::<Vec<_>>();
            assert_eq!(
                heroes.len(),
                1,
                "actual Job select token from paid source ETB"
            );
            assert_eq!(effective_pt(&mut runner, heroes[0]), (1, 1));
            assert!(runner.state().objects[&heroes[0]]
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Assassin"));
        }
        assert_continuous_complete(runner.state());
        complete_publication_fund(&mut runner, P0, if scythe { 2 } else { 3 }, &[]);
        runner
            .act(GameAction::Equip {
                equipment_id: source,
                target_id: host,
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(effective_pt(&mut runner, host), (2, 3));
        if scythe {
            assert!(runner.state().objects[&host]
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Assassin"));
        } else {
            assert!(runner.state().objects[&host]
                .keywords
                .contains(&Keyword::Vigilance));
        }
        assert_eq!(effective_pt(&mut runner, foreign), (4, 6));
        assert_continuous_complete(runner.state());
        if scythe {
            complete_publication_fund(&mut runner, P0, 0, &[ManaType::Red]);
            runner.cast(bolt).target_player(P1).resolve();
            assert_continuous_complete(runner.state());
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            runner.advance_until_stack_empty();
        } else {
            complete_publication_fund(&mut runner, P0, 1, &[ManaType::Green]);
            runner.cast(entrant).resolve();
        }
        let counter = CounterType::Generic(if scythe { "soul" } else { "charge" }.into());
        assert_eq!(
            runner.state().objects[&source].counters.get(&counter),
            Some(&1)
        );
        assert_eq!(
            effective_pt(&mut runner, host),
            (3, 4),
            "actual ETB/end-step source counter updates both axes"
        );
        assert_eq!(effective_pt(&mut runner, foreign), (4, 6));
        assert_continuous_complete(runner.state());
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
        runner.cast(bounce).target_object(source).resolve();
        assert_eq!(effective_pt(&mut runner, host), (2, 3));
        if scythe {
            assert!(!runner.state().objects[&host]
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Assassin"));
        } else {
            assert!(!runner.state().objects[&host]
                .keywords
                .contains(&Keyword::Vigilance));
        }
        assert_continuous_complete(runner.state());
    }
}
#[test]
fn complete_continuous_source_history_noncombat_zero_and_later_turns() {
    use engine::types::game_state::CastPaymentMode;
    for zero in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(P0, "Oyaminartok, Polar Werebear", 4, 5, OYAMINARTOK)
            .as_legendary()
            .with_subtypes(vec!["Giant", "Bear"])
            .with_mana_cost(complete_publication_cost("{2}{G}{G}"))
            .id();
        let foreign = scenario
            .add_creature_from_oracle(P1, "Oyaminartok, Polar Werebear", 4, 5, OYAMINARTOK)
            .as_legendary()
            .with_subtypes(vec!["Giant", "Bear"])
            .with_mana_cost(complete_publication_cost("{2}{G}{G}"))
            .id();
        let fire = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Soul's Fire",
                true,
                "Target creature you control deals damage equal to its power to any target.",
            )
            .with_mana_cost(complete_publication_cost("{2}{R}"))
            .id();
        let surge = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Hydrosurge",
                true,
                "Target creature gets -5/-0 until end of turn.",
            )
            .with_mana_cost(complete_publication_cost("{U}"))
            .id();
        let blink = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true,
            "Exile target creature you control, then return that card to the battlefield under your control.")
            .with_mana_cost(complete_publication_cost("{W}")).id();
        let steal = scenario.add_spell_to_hand_from_oracle(P1, "Turn Against", true,
            "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.")
            .with_mana_cost(complete_publication_cost("{4}{R}")).id();
        for player in [P0, P1] {
            for _ in 0..8 {
                scenario.add_card_to_library_top(player, "Draw buffer");
            }
        }
        let mut runner = scenario.build();
        assert_eq!(effective_pt(&mut runner, source), (4, 5));
        assert!(runner.state().objects[&source]
            .keywords
            .contains(&Keyword::Hexproof));
        if zero {
            complete_publication_fund(&mut runner, P0, 0, &[ManaType::Blue]);
            runner.cast(surge).target_object(source).resolve();
            assert_eq!(effective_pt(&mut runner, source), (-1, 5));
            assert_continuous_complete(runner.state());
        }
        let before = runner.state().players[1].life;
        complete_publication_fund(&mut runner, P0, 2, &[ManaType::Red]);
        runner
            .cast(fire)
            .target_object(source)
            .target_player(P1)
            .resolve();
        assert_eq!(
            runner.state().players[1].life,
            before - if zero { 0 } else { 4 }
        );
        assert_eq!(
            runner.state().objects[&source]
                .keywords
                .contains(&Keyword::Hexproof),
            zero
        );
        assert!(runner.state().objects[&foreign]
            .keywords
            .contains(&Keyword::Hexproof));
        assert!(
            !runner
                .state()
                .battlefield
                .iter()
                .any(|id| runner.state().objects[id]
                    .card_types
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Food")),
            "noncombat and zero damage never create combat-damage Food"
        );
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().active_player, P1);
        assert_eq!(effective_pt(&mut runner, source), (4, 5));
        assert_eq!(
            runner.state().objects[&source]
                .keywords
                .contains(&Keyword::Hexproof),
            zero,
            "new turn does not reset source history"
        );
        assert_continuous_complete(runner.state());
        let stable_epoch = runner.state().objects[&source].incarnation;
        complete_publication_fund(&mut runner, P1, 4, &[ManaType::Red]);
        runner
            .act(GameAction::CastSpell {
                object_id: steal,
                card_id: runner.state().objects[&steal].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::default(),
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real history-sensitive control target")
        };
        assert_eq!(*player, P1);
        assert_eq!(target_slots.len(), 1);
        let slot = &target_slots[selection.current_slot];
        assert!(!slot.optional);
        assert!(
            slot.legal_targets.contains(&TargetRef::Object(foreign)),
            "own second authority is always legal"
        );
        assert_eq!(
            slot.legal_targets.contains(&TargetRef::Object(source)),
            !zero
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(if zero { foreign } else { source })),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&source].owner, P0);
        assert_eq!(
            runner.state().objects[&source].controller,
            if zero { P0 } else { P1 }
        );
        assert_eq!(runner.state().objects[&source].incarnation, stable_epoch);
        assert_eq!(
            runner.state().objects[&source]
                .keywords
                .contains(&Keyword::Hexproof),
            zero
        );
        assert!(runner.state().objects[&foreign]
            .keywords
            .contains(&Keyword::Hexproof));
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().active_player, P0);
        assert_eq!(runner.state().objects[&source].controller, P0);
        assert_eq!(
            runner.state().objects[&source]
                .keywords
                .contains(&Keyword::Hexproof),
            zero
        );
        let epoch = runner.state().objects[&source].incarnation;
        complete_publication_fund(&mut runner, P0, 0, &[ManaType::White]);
        runner.cast(blink).target_object(source).resolve();
        assert_ne!(runner.state().objects[&source].incarnation, epoch);
        assert!(runner.state().objects[&source]
            .keywords
            .contains(&Keyword::Hexproof));
        assert!(runner.state().objects[&foreign]
            .keywords
            .contains(&Keyword::Hexproof));
        assert_continuous_complete(runner.state());
    }
}
#[test]
fn complete_continuous_native_loyalty_members_pay_counters_and_outlive_source() {
    for (name, subtype, printed_loyalty, paid_loyalty, cost, text, size) in [
        ("Greensleeves", "Greensleeves", 5, 8, "{4}{G}{G}", "[+2]: Create a Giant Badger token. (It's a {1}{G}{G} 2/2 Badger creature with \"Whenever this token blocks, it gets +2/+2 until end of turn.\")\n[−3]: Mill three cards. Put all permanent cards from among them into your hand.\n[−8]: Until end of turn, creatures you control have base power and toughness 8/8 and gain trample.\nGreensleeves can be your commander.", 8),
        ("Oko, the Trickster", "Oko", 4, 7, "{4}{G}{U}", "[+1]: Put two +1/+1 counters on up to one target creature you control.\n[0]: Until end of turn, Oko becomes a copy of target creature you control. Prevent all damage that would be dealt to him this turn.\n[−7]: Until end of turn, each creature you control has base power and toughness 10/10 and gains trample.", 10),
    ] {
        let mut scenario = GameScenario::new(); scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_planeswalker_from_oracle(P0, name, subtype, printed_loyalty, text)
            .as_legendary().with_mana_cost(complete_publication_cost(cost)).id();
        scenario.with_counter(source, CounterType::Loyalty, paid_loyalty);
        let own = scenario.add_creature(P0, "Countered unequal recipient", 2, 7).with_plus_counters(1).id();
        let other = scenario.add_creature(P0, "Other unequal recipient", 4, 6).id();
        let foreign = scenario.add_creature(P1, "Foreign recipient", 6, 8).id();
        let entrant = scenario.add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
            .with_subtypes(vec!["Bear"]).with_mana_cost(complete_publication_cost("{1}{G}")).id();
        for player in [P0, P1] { for _ in 0..4 { scenario.add_card_to_library_top(player, "Draw buffer"); } }
        let mut runner = scenario.build();
        runner.activate(source, 2).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard, "actual loyalty cost consumes the source's last counters");
        assert_eq!(effective_pt(&mut runner, own), (size+1,size+1)); assert_eq!(effective_pt(&mut runner, other), (size,size));
        for object in [own,other] { assert!(runner.state().objects[&object].keywords.contains(&Keyword::Trample)); }
        assert_eq!(effective_pt(&mut runner, foreign), (6,8)); assert!(!runner.state().objects[&foreign].keywords.contains(&Keyword::Trample));
        assert_continuous_complete(runner.state());
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Green]); runner.cast(entrant).resolve();
        assert_eq!(effective_pt(&mut runner, entrant), (2,2)); assert!(!runner.state().objects[&entrant].keywords.contains(&Keyword::Trample));
        assert_eq!(effective_pt(&mut runner, own), (size+1,size+1)); assert_continuous_complete(runner.state());
        runner.advance_to_combat(); runner.declare_attackers(&[]).unwrap(); runner.advance_to_end_step(); runner.pass_both_players();
        assert_eq!(effective_pt(&mut runner, own), (3,8)); assert_eq!(effective_pt(&mut runner, other), (4,6));
        for object in [own,other,entrant] { assert!(!runner.state().objects[&object].keywords.contains(&Keyword::Trample)); }
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_dragonblood_second_cast_and_eidolon_actual_entry() {
    for eidolon in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let text = if eidolon {
            "Vigilance\nConstellation — Whenever this creature or another enchantment you control enters, choose target creature you control. Until end of turn, that creature has base power and toughness 4/4 and gains flying."
        } else {
            "Double team\nFlurry — Whenever you cast your second spell each turn, until end of turn, this creature has base power and toughness 4/4 and gains flying."
        };
        let source = if eidolon {
            scenario
                .add_creature_to_hand_from_oracle(P0, "Eidolon of Astral Winds", 2, 4, text)
                .as_enchantment()
                .as_creature()
                .with_subtypes(vec!["Spirit"])
                .with_mana_cost(complete_publication_cost("{2}{W}"))
                .id()
        } else {
            scenario
                .add_creature_from_oracle(P0, "Dragonblood Twins", 2, 2, text)
                .with_subtypes(vec!["Human", "Monk"])
                .with_mana_cost(complete_publication_cost("{1}{R}"))
                .id()
        };
        let own = scenario
            .add_creature(P0, "Unequal chosen creature", 2, 7)
            .with_plus_counters(1)
            .id();
        let other = scenario.add_creature(P0, "Other legal creature", 6, 8).id();
        let foreign = scenario
            .add_creature(P1, "Foreign illegal recipient", 5, 6)
            .id();
        let first = scenario
            .add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(complete_publication_cost("{1}{G}"))
            .id();
        let second = scenario
            .add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(complete_publication_cost("{1}{G}"))
            .id();
        for player in [P0, P1] {
            for _ in 0..4 {
                scenario.add_card_to_library_top(player, "Draw buffer");
            }
        }
        let mut runner = scenario.build();
        let recipient = if eidolon { own } else { source };
        if eidolon {
            complete_publication_fund(&mut runner, P0, 2, &[ManaType::White]);
            runner.cast(source).commit();
            runner.pass_both_players();
            let WaitingFor::TriggerTargetSelection {
                player,
                source_id,
                target_slots,
                selection,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("actual Constellation ETB target")
            };
            assert_eq!(*player, P0);
            assert_eq!(*source_id, Some(source));
            assert_eq!(target_slots.len(), 1);
            let slot = &target_slots[selection.current_slot];
            assert!(!slot.optional);
            assert!(slot.legal_targets.contains(&TargetRef::Object(own)));
            assert!(slot.legal_targets.contains(&TargetRef::Object(other)));
            assert!(!slot.legal_targets.contains(&TargetRef::Object(foreign)));
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(own)),
                })
                .unwrap();
            runner.advance_until_stack_empty();
        } else {
            complete_publication_fund(&mut runner, P0, 2, &[ManaType::Green, ManaType::Green]);
            runner.cast(first).resolve();
            assert_eq!(effective_pt(&mut runner, source), (2, 2));
            assert!(!runner.state().objects[&source]
                .keywords
                .contains(&Keyword::Flying));
            assert_continuous_complete(runner.state());
            runner.cast(second).commit();
            assert_eq!(runner.state().stack.len(), 2);
            assert_eq!(
                runner
                    .state()
                    .stack
                    .last()
                    .unwrap()
                    .ability()
                    .unwrap()
                    .source_id,
                source
            );
            runner.advance_until_stack_empty();
        }
        assert_eq!(
            effective_pt(&mut runner, recipient),
            if eidolon { (5, 5) } else { (4, 4) }
        );
        assert!(runner.state().objects[&recipient]
            .keywords
            .contains(&Keyword::Flying));
        assert_eq!(effective_pt(&mut runner, other), (6, 8));
        assert!(!runner.state().objects[&other]
            .keywords
            .contains(&Keyword::Flying));
        assert_eq!(effective_pt(&mut runner, foreign), (5, 6));
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        assert_eq!(
            effective_pt(&mut runner, recipient),
            if eidolon { (3, 8) } else { (2, 2) }
        );
        assert!(!runner.state().objects[&recipient]
            .keywords
            .contains(&Keyword::Flying));
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_flayer_paid_cast_trigger_preserves_control_untap_and_companions() {
    use engine::types::game_state::CastPaymentMode;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source=scenario.add_creature_to_hand_from_oracle(P0,"Flayer of Loyalties",10,10,
        "When you cast this spell, gain control of target creature until end of turn. Untap that creature. Until end of turn, it has base power and toughness 10/10 and gains trample, annihilator 2, and haste.\nAnnihilator 2 (Whenever this creature attacks, defending player sacrifices two permanents of their choice.)\nTrample")
        .with_subtypes(vec!["Eldrazi"]).with_mana_cost(complete_publication_cost("{8}{C}{C}")).id();
    let chosen = scenario
        .add_creature(P1, "Chosen foreign creature", 2, 7)
        .id();
    let other = scenario
        .add_creature(P1, "Other foreign creature", 4, 6)
        .id();
    for player in [P0, P1] {
        for _ in 0..4 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    scenario.with_mana_pool(P0, complete_publication_pool(10, &[]));
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&chosen).unwrap().tapped = true;
    runner
        .act(GameAction::CastSpell {
            object_id: source,
            card_id: runner.state().objects[&source].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::default(),
        })
        .unwrap();
    let WaitingFor::TriggerTargetSelection {
        player,
        source_id,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "actual Flayer cast target: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, Some(source));
    assert_eq!(target_slots.len(), 1);
    let slot = &target_slots[selection.current_slot];
    assert!(!slot.optional);
    assert!(slot.legal_targets.contains(&TargetRef::Object(chosen)));
    assert!(slot.legal_targets.contains(&TargetRef::Object(other)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().objects[&chosen].owner, P1);
    assert_eq!(runner.state().objects[&chosen].controller, P0);
    assert!(!runner.state().objects[&chosen].tapped);
    assert_eq!(effective_pt(&mut runner, chosen), (10, 10));
    for keyword in [Keyword::Trample, Keyword::Annihilator(2), Keyword::Haste] {
        assert!(runner.state().objects[&chosen].keywords.contains(&keyword));
    }
    assert_eq!(effective_pt(&mut runner, other), (4, 6));
    assert_eq!(runner.state().objects[&other].controller, P1);
    assert_continuous_complete(runner.state());
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    assert_eq!(effective_pt(&mut runner, chosen), (2, 7));
    assert_eq!(runner.state().objects[&chosen].controller, P1);
    for keyword in [Keyword::Trample, Keyword::Annihilator(2), Keyword::Haste] {
        assert!(!runner.state().objects[&chosen].keywords.contains(&keyword));
    }
    assert!(runner.state().objects[&source]
        .keywords
        .contains(&Keyword::Trample));
    assert_continuous_complete(runner.state());
}
#[test]
fn complete_continuous_azula_experience_is_controller_scoped_and_wings_targets_either_controller() {
    use engine::types::player::PlayerCounterKind;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    const AZULA:&str="Firebending 1 (Whenever this creature attacks, add {R}. This mana lasts until end of combat.)\nWhenever Azula attacks, you may discard a card. Then you get an experience counter for each player who discarded a card this turn.\n{2}{B}: Until end of turn, Azula gets +1/+1 for each experience counter you have and gains menace.";
    let source = scenario
        .add_creature_from_oracle(P0, "Azula, Ruthless Firebender", 3, 3, AZULA)
        .as_legendary()
        .with_subtypes(vec!["Human", "Noble"])
        .with_mana_cost(complete_publication_cost("{2}{B}"))
        .id();
    let foreign = scenario
        .add_creature_from_oracle(P1, "Azula, Ruthless Firebender", 3, 3, AZULA)
        .as_legendary()
        .with_subtypes(vec!["Human", "Noble"])
        .with_mana_cost(complete_publication_cost("{2}{B}"))
        .id();
    for player in [P0, P1] {
        for _ in 0..4 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().players[0]
        .player_counters
        .insert(PlayerCounterKind::Experience, 2);
    runner.state_mut().players[1]
        .player_counters
        .insert(PlayerCounterKind::Experience, 5);
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Black]);
    runner.activate(source, 0).resolve();
    assert_eq!(effective_pt(&mut runner, source), (5, 5));
    assert!(runner.state().objects[&source]
        .keywords
        .contains(&Keyword::Menace));
    assert_eq!(effective_pt(&mut runner, foreign), (3, 3));
    assert!(!runner.state().objects[&foreign]
        .keywords
        .contains(&Keyword::Menace));
    assert_continuous_complete(runner.state());
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    assert_eq!(effective_pt(&mut runner, source), (3, 3));
    assert!(!runner.state().objects[&source]
        .keywords
        .contains(&Keyword::Menace));
    assert_continuous_complete(runner.state());
    for owner in [P0, P1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let chosen = scenario
            .add_creature(owner, "Unequal countered target", 2, 7)
            .with_plus_counters(1)
            .with_subtypes(vec!["Goblin"])
            .id();
        let other = scenario.add_creature(P0, "Untargeted control", 4, 6).id();
        let spell=scenario.add_spell_to_hand_from_oracle(P0,"Wings of Velis Vel",true,
            "Changeling (This card is every creature type.)\nUntil end of turn, target creature has base power and toughness 4/4, gains all creature types, and gains flying.")
            .with_subtypes(vec!["Shapeshifter"]).with_mana_cost(complete_publication_cost("{1}{U}")).id();
        for player in [P0, P1] {
            for _ in 0..4 {
                scenario.add_card_to_library_top(player, "Draw buffer");
            }
        }
        let mut runner = scenario.build();
        // Printed kindred is fixture setup; neither a target nor an effect is seeded.
        let object = runner.state_mut().objects.get_mut(&spell).unwrap();
        object
            .card_types
            .core_types
            .push(engine::types::card_type::CoreType::Kindred);
        object.base_card_types = object.card_types.clone();
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
        runner.cast(spell).target_object(chosen).resolve();
        assert_eq!(effective_pt(&mut runner, chosen), (5, 5));
        for keyword in [Keyword::Changeling, Keyword::Flying] {
            assert!(runner.state().objects[&chosen].keywords.contains(&keyword));
        }
        assert!(runner.state().objects[&chosen].card_types.subtypes.len() > 20);
        assert_eq!(effective_pt(&mut runner, other), (4, 6));
        assert!(!runner.state().objects[&other]
            .keywords
            .contains(&Keyword::Flying));
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        assert_eq!(effective_pt(&mut runner, chosen), (3, 8));
        for keyword in [Keyword::Changeling, Keyword::Flying] {
            assert!(!runner.state().objects[&chosen].keywords.contains(&keyword));
        }
        assert_continuous_complete(runner.state());
    }
}
#[test]
fn complete_continuous_elturel_paid_cast_myriad_accept_and_two_player_empty_population() {
    use engine::game::combat::AttackTarget;
    let p2 = PlayerId(2);
    for seats in [2, 3] {
        let mut scenario = GameScenario::new_n_player(seats, 2020);
        scenario.at_phase(Phase::PreCombatMain);
        for _ in 0..2 {
            scenario.add_basic_land(P1, ManaColor::Green);
        }
        if seats == 3 {
            for _ in 0..5 {
                scenario.add_basic_land(p2, ManaColor::Blue);
            }
        }
        let source = scenario
            .add_creature_to_hand_from_oracle(P0, "Elturel Survivors", 0, 4, ELTUREL)
            .with_subtypes(vec!["Tiefling", "Peasant"])
            .with_color(vec![ManaColor::Red])
            .with_mana_cost(complete_publication_cost("{3}{R}"))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{U}"))
            .id();
        for seat in 0..seats {
            let player = PlayerId(seat);
            scenario.with_life(player, 100);
            for _ in 0..8 {
                scenario.add_card_to_library_top(player, "Draw buffer");
            }
        }
        let mut runner = scenario.build();
        complete_publication_fund(&mut runner, P0, 3, &[ManaType::Red]);
        runner.cast(source).resolve();
        assert_eq!(effective_pt(&mut runner, source), (0, 4));
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_continuous_complete(runner.state());
        // The real paid creature waits through a complete cycle before attacking.
        for seat in 0..seats {
            assert_eq!(runner.state().active_player, PlayerId(seat));
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            for _ in 0..seats {
                runner.act(GameAction::PassPriority).unwrap();
            }
            runner.advance_to_phase(Phase::PreCombatMain);
            assert_continuous_complete(runner.state());
        }
        assert_eq!(runner.state().active_player, P0);
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(source, AttackTarget::Player(P1))])
            .unwrap();
        runner.advance_until_stack_empty();
        if seats == 3 {
            let WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } = &runner.state().waiting_for
            else {
                panic!("real Myriad decision")
            };
            assert_eq!(*player, P0);
            assert_eq!(*source_id, source);
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
            runner.advance_until_stack_empty();
        }
        assert_eq!(effective_pt(&mut runner, source), (2, 4));
        let copies = runner
            .state()
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                runner.state().objects[id].is_token
                    && runner.state().objects[id].name == "Elturel Survivors"
            })
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), usize::from(seats == 3));
        if seats == 3 {
            let token = copies[0];
            assert_eq!(effective_pt(&mut runner, token), (5, 4));
            assert!(runner.state().objects[&token].tapped);
            assert!(runner.state().objects[&token]
                .keywords
                .contains(&Keyword::Trample));
            let attacker = runner
                .state()
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .find(|attacker| attacker.object_id == token)
                .unwrap();
            assert_eq!(attacker.defending_player, p2);
            complete_publication_fund(&mut runner, P0, 0, &[ManaType::Blue]);
            runner.cast(bounce).target_object(source).resolve();
            assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
            assert_eq!(
                effective_pt(&mut runner, source),
                (0, 4),
                "conditional attacking static is not an all-zone CDA"
            );
            assert_eq!(
                effective_pt(&mut runner, token),
                (5, 4),
                "real Myriad token retains its own defender and lifetime after source leaves"
            );
        }
        assert_continuous_complete(runner.state());
        runner.advance_to_phase(Phase::DeclareBlockers);
        for seat in 1..seats {
            let WaitingFor::DeclareBlockers { player, .. } = runner.state().waiting_for else {
                panic!("actual defender blockers")
            };
            assert_eq!(player, PlayerId(seat));
            runner.declare_blockers(&[]).unwrap();
        }
        runner.advance_to_phase(Phase::PostCombatMain);
        runner.advance_until_stack_empty();
        assert_eq!(effective_pt(&mut runner, source), (0, 4));
        for token in copies {
            assert!(!runner.state().battlefield.contains(&token));
            assert!(runner
                .state()
                .objects
                .get(&token)
                .is_none_or(|object| object.zone != Zone::Battlefield));
        }
        assert_continuous_complete(runner.state());
    }
}
#[test]
fn complete_continuous_aurelia_two_actual_sources_keep_controller_and_recipient_authorities() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_from_oracle(P0, "Aurelia, Exemplar of Justice", 2, 5, AURELIA)
        .as_legendary()
        .with_subtypes(vec!["Angel"])
        .with_color(vec![ManaColor::Red, ManaColor::White])
        .with_mana_cost(complete_publication_cost("{2}{R}{W}"))
        .id();
    let second = scenario
        .add_creature_from_oracle(P1, "Aurelia, Exemplar of Justice", 2, 5, AURELIA)
        .as_legendary()
        .with_subtypes(vec!["Angel"])
        .with_color(vec![ManaColor::Red, ManaColor::White])
        .with_mana_cost(complete_publication_cost("{2}{R}{W}"))
        .id();
    let red = scenario
        .add_creature(P0, "Red chosen recipient", 2, 2)
        .with_color(vec![ManaColor::Red])
        .id();
    let white = scenario
        .add_creature(P1, "White chosen recipient", 3, 4)
        .with_color(vec![ManaColor::White])
        .id();
    for player in [P0, P1] {
        for _ in 0..6 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    for (actor, source, chosen, foreign, expected, keyword) in [
        (P0, first, red, white, (4, 2), Keyword::Trample),
        (P1, second, white, red, (5, 4), Keyword::Vigilance),
    ] {
        runner.advance_to_phase(Phase::BeginCombat);
        let WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual source-specific BeginCombat")
        };
        assert_eq!(*player, actor);
        assert_eq!(*source_id, Some(source));
        assert_eq!(target_slots.len(), 1);
        let slot = &target_slots[selection.current_slot];
        assert!(slot.optional);
        assert!(slot.legal_targets.contains(&TargetRef::Object(chosen)));
        assert!(slot.legal_targets.contains(&TargetRef::Object(source)));
        assert!(!slot.legal_targets.contains(&TargetRef::Object(foreign)));
        assert!(runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(foreign))
            })
            .is_err());
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(chosen)),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(effective_pt(&mut runner, chosen), expected);
        assert!(runner.state().objects[&chosen].keywords.contains(&keyword));
        assert_eq!(
            runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Trample),
            actor == P0
        );
        assert_eq!(
            runner.state().objects[&chosen]
                .keywords
                .contains(&Keyword::Vigilance),
            actor == P1
        );
        assert_eq!(
            effective_pt(&mut runner, foreign),
            if actor == P0 { (3, 4) } else { (2, 2) }
        );
        for object in [first, second] {
            assert_eq!(effective_pt(&mut runner, object), (2, 5));
        }
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        assert_eq!(effective_pt(&mut runner, red), (2, 2));
        assert_eq!(effective_pt(&mut runner, white), (3, 4));
        assert!(!runner.state().objects[&red]
            .keywords
            .contains(&Keyword::Trample));
        assert!(!runner.state().objects[&white]
            .keywords
            .contains(&Keyword::Vigilance));
        assert_continuous_complete(runner.state());
        runner.advance_to_phase(Phase::PreCombatMain);
    }
}
#[test]
fn complete_continuous_jodah_real_legendary_cast_offer_preserves_random_bottom_siblings() {
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let jodah = scenario
            .add_creature_from_oracle(P0, "Jodah, the Unifier", 5, 5, JODAH)
            .as_legendary()
            .with_subtypes(vec!["Human", "Wizard"])
            .with_mana_cost(complete_publication_cost("{W}{U}{B}{R}{G}"))
            .id();
        let hancock = scenario
            .add_creature_to_hand_from_oracle(P0, "Hancock, Ghoulish Mayor", 2, 1, HANCOCK)
            .as_legendary()
            .with_subtypes(vec!["Zombie", "Mutant", "Advisor"])
            .with_mana_cost(complete_publication_cost("{2}{B}"))
            .id();
        let unreached = scenario
            .add_land_to_library_top(P0, "Island")
            .with_subtypes(vec!["Island"])
            .id();
        let hit = scenario
            .add_spell_to_library_top(P0, "Isamaru, Hound of Konda", false)
            .as_creature()
            .as_legendary()
            .with_subtypes(vec!["Dog"])
            .with_mana_cost(complete_publication_cost("{W}"))
            .id();
        let miss_one = scenario
            .add_land_to_library_top(P0, "Forest")
            .with_subtypes(vec!["Forest"])
            .id();
        let miss_two = scenario
            .add_land_to_library_top(P0, "Mountain")
            .with_subtypes(vec!["Mountain"])
            .id();
        let mut runner = scenario.build();
        // Exact printed characteristics of the initial library fixture.
        let object = runner.state_mut().objects.get_mut(&hit).unwrap();
        object.base_power = Some(2);
        object.base_toughness = Some(2);
        object.power = Some(2);
        object.toughness = Some(2);
        assert_eq!(effective_pt(&mut runner, jodah), (6, 6));
        complete_publication_fund(&mut runner, P0, 2, &[ManaType::Black]);
        runner.cast(hancock).commit().stop_at_optional().resolve();
        let WaitingFor::OptionalEffectChoice {
            player, source_id, ..
        } = &runner.state().waiting_for
        else {
            panic!("actual Jodah cast offer")
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, jodah);
        assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&miss_one].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&miss_two].zone, Zone::Exile);
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&hancock].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&hit].zone,
            if accept {
                Zone::Battlefield
            } else {
                Zone::Exile
            }
        );
        assert_eq!(
            effective_pt(&mut runner, jodah),
            if accept { (8, 8) } else { (7, 7) }
        );
        assert_eq!(
            effective_pt(&mut runner, hancock),
            if accept { (5, 4) } else { (4, 3) }
        );
        let library = runner.state().players[0]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(library.len(), 3);
        assert_eq!(library[0], unreached);
        assert!(library[1..].contains(&miss_one));
        assert!(library[1..].contains(&miss_two));
        assert_eq!(runner.state().objects[&miss_one].zone, Zone::Library);
        assert_eq!(runner.state().objects[&miss_two].zone, Zone::Library);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn complete_continuous_zinnia_real_offspring_zero_one_and_decrease() {
    use engine::types::ability::AdditionalCostOrigin;
    use engine::types::game_state::CastPaymentMode;
    for pay in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let zinnia = scenario
            .add_creature_from_oracle(P0, "Zinnia, Valley's Voice", 1, 3, ZINNIA)
            .as_legendary()
            .with_subtypes(vec!["Bird", "Bard"])
            .with_mana_cost(complete_publication_cost("{U}{R}{W}"))
            .id();
        let foreign = scenario
            .add_creature(P1, "Foreign base-one control", 1, 4)
            .id();
        let parent = scenario
            .add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(complete_publication_cost("{1}{G}"))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Disperse",
                true,
                "Return target nonland permanent to its owner's hand.",
            )
            .with_mana_cost(complete_publication_cost("{1}{U}"))
            .id();
        let mut runner = scenario.build();
        assert_eq!(effective_pt(&mut runner, zinnia), (1, 3));
        complete_publication_fund(&mut runner, P0, if pay { 3 } else { 1 }, &[ManaType::Green]);
        runner
            .act(GameAction::CastSpell {
                object_id: parent,
                card_id: runner.state().objects[&parent].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::default(),
            })
            .unwrap();
        let WaitingFor::OptionalCostChoice {
            player,
            origin,
            times_kicked,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual cast-time granted Offspring")
        };
        assert_eq!(*player, P0);
        assert_eq!(*origin, AdditionalCostOrigin::Offspring);
        assert_eq!(*times_kicked, 0);
        runner.act(GameAction::DecideOptionalCost { pay }).unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().objects[&parent].zone, Zone::Battlefield);
        assert_eq!(effective_pt(&mut runner, parent), (2, 2));
        let offspring = runner
            .state()
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                runner.state().objects[id].is_token
                    && runner.state().objects[id].name == "Runeclaw Bear"
            })
            .collect::<Vec<_>>();
        assert_eq!(offspring.len(), usize::from(pay));
        if pay {
            assert_eq!(effective_pt(&mut runner, offspring[0]), (1, 1));
        }
        assert_eq!(
            effective_pt(&mut runner, zinnia),
            if pay { (2, 3) } else { (1, 3) }
        );
        assert_eq!(effective_pt(&mut runner, foreign), (1, 4));
        assert_continuous_complete(runner.state());
        if pay {
            complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
            runner.cast(bounce).target_object(offspring[0]).resolve();
            assert!(!runner.state().battlefield.contains(&offspring[0]));
            assert_eq!(effective_pt(&mut runner, zinnia), (1, 3));
            assert!(runner.state().objects[&zinnia]
                .keywords
                .contains(&Keyword::Flying));
            assert_continuous_complete(runner.state());
        }
    }
}

#[test]
fn complete_continuous_almost_no_legal_target_two_sources_and_paid_reentry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Almost Perfect", false, ALMOST)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(complete_publication_cost("{4}{G}{W}"))
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Almost Perfect", false, ALMOST)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(complete_publication_cost("{4}{G}{W}"))
        .id();
    let host = scenario
        .add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(complete_publication_cost("{1}{G}"))
        .id();
    let next = scenario
        .add_creature_to_hand_from_oracle(P0, "Runeclaw Bear", 2, 2, "")
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(complete_publication_cost("{1}{G}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    let mut runner = scenario.build();
    complete_publication_fund(&mut runner, P0, 4, &[ManaType::Green, ManaType::White]);
    assert!(
        runner.cast(first).try_resolve().is_err(),
        "funded Aura still needs a legal creature"
    );
    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].mana_pool.total(), 6);
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Green, ManaType::Green]);
    runner.cast(host).resolve();
    runner.cast(next).resolve();
    runner.cast(first).target_object(host).resolve();
    assert_eq!(effective_pt(&mut runner, host), (9, 10));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(
        runner
            .cast(second)
            .target_object(host)
            .try_resolve()
            .is_err(),
        "a proven legal Aura target cannot bypass the unfunded printed cost"
    );
    assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 4, &[ManaType::Green, ManaType::White]);
    runner.cast(second).target_object(host).resolve();
    assert_eq!(effective_pt(&mut runner, host), (9, 10));
    assert!(runner.state().objects[&host]
        .keywords
        .contains(&Keyword::Indestructible));
    assert_eq!(effective_pt(&mut runner, next), (2, 2));
    assert!(!runner.state().objects[&next]
        .keywords
        .contains(&Keyword::Indestructible));
    let epoch = runner.state().objects[&first].incarnation;
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(first).resolve();
    assert_eq!(effective_pt(&mut runner, host), (9, 10));
    assert!(runner.state().objects[&host]
        .keywords
        .contains(&Keyword::Indestructible));
    complete_publication_fund(&mut runner, P0, 4, &[ManaType::Green, ManaType::White]);
    runner.cast(first).target_object(next).resolve();
    assert_ne!(runner.state().objects[&first].incarnation, epoch);
    for object in [host, next] {
        assert_eq!(effective_pt(&mut runner, object), (9, 10));
        assert!(runner.state().objects[&object]
            .keywords
            .contains(&Keyword::Indestructible));
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_continuous_complete(runner.state());
}
#[test]
fn complete_continuous_rakdos_granter_reentry_invalidates_only_old_activation() {
    use engine::types::game_state::CastPaymentMode;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knife = scenario
        .add_artifact_from_oracle(P0, "Rakdos Riteknife", RITEKNIFE)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(complete_publication_cost("{2}"))
        .id();
    let decoy = scenario
        .add_artifact_from_oracle(P0, "Rakdos Riteknife", RITEKNIFE)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(complete_publication_cost("{2}"))
        .id();
    scenario.with_counter(decoy, CounterType::Generic("blood".into()), 5);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let host = scenario.add_creature(P0, "Granted host", 2, 3).id();
    let fodder = scenario.add_creature(P0, "Sacrifice choice", 1, 1).id();
    let flicker=scenario.add_spell_to_hand_from_oracle(P0,"Ghostly Flicker",true,
        "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.")
        .with_mana_cost(complete_publication_cost("{2}{U}")).id();
    let untap = scenario
        .add_spell_to_hand_from_oracle(P0, "Refocus", true, "Untap target creature.\nDraw a card.")
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    scenario.add_card_to_library_top(P0, "Draw buffer");
    let mut runner = scenario.build();
    complete_publication_fund(&mut runner, P0, 2, &[]);
    runner
        .act(GameAction::Equip {
            equipment_id: knife,
            target_id: host,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    let old_epoch = runner.state().objects[&knife].incarnation;
    let index = runner.state().objects[&host]
        .abilities
        .iter()
        .position(|definition| {
            definition
                .granting_object
                .is_some_and(|object| object.object_id == knife)
        })
        .unwrap();
    runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: host,
            ability_index: index,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("real sacrifice cost")
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert!(choices.contains(&host));
    assert!(choices.contains(&fodder));
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::Blue]);
    runner
        .act(GameAction::CastSpell {
            object_id: flicker,
            card_id: runner.state().objects[&flicker].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::default(),
        })
        .unwrap();
    for chosen in [land, knife] {
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real two-object Flicker announcement")
        };
        assert_eq!(*player, P0);
        assert_eq!(target_slots.len(), 2);
        assert!(!target_slots[selection.current_slot].optional);
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(chosen)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(chosen)),
            })
            .unwrap();
    }
    assert_eq!(runner.state().stack.len(), 2);
    runner.resolve_top();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "old granted activation remains after real Flicker"
    );
    assert_eq!(runner.state().stack[0].ability().unwrap().source_id, host);
    assert_ne!(runner.state().objects[&knife].incarnation, old_epoch);
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects[&knife]
        .counters
        .contains_key(&CounterType::Generic("blood".into())));
    assert_eq!(
        runner.state().objects[&decoy]
            .counters
            .get(&CounterType::Generic("blood".into())),
        Some(&5)
    );
    assert_eq!(effective_pt(&mut runner, host), (2, 3));
    assert!(runner.state().objects[&host].tapped);
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 3, &[ManaType::Blue]);
    runner
        .act(GameAction::Equip {
            equipment_id: knife,
            target_id: host,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    runner.cast(untap).target_object(host).resolve();
    assert!(!runner.state().objects[&host].tapped);
    assert_continuous_complete(runner.state());
    let new_index = runner.state().objects[&host]
        .abilities
        .iter()
        .position(|definition| {
            definition
                .granting_object
                .is_some_and(|object| object.object_id == knife)
        })
        .unwrap();
    runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: host,
            ability_index: new_index,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("new actual grant cost")
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(choices, &vec![host]);
    runner
        .act(GameAction::SelectCards { cards: vec![host] })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&host].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&knife]
            .counters
            .get(&CounterType::Generic("blood".into())),
        Some(&1)
    );
    assert_eq!(
        runner.state().objects[&decoy]
            .counters
            .get(&CounterType::Generic("blood".into())),
        Some(&5)
    );
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_equipment_control_is_independent_of_recipient_and_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let equipment = scenario
        .add_artifact_from_oracle(P1, "Mace of the Valiant", MACE)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(complete_publication_cost("{2}{W}"))
        .id();
    scenario.with_counter(equipment, CounterType::Generic("charge".into()), 2);
    scenario.with_counter(equipment, CounterType::Generic("blood".into()), 7);
    let old_host = scenario
        .add_creature(P1, "Existing foreign host", 2, 3)
        .id();
    let own_host = scenario.add_creature(P0, "New host", 4, 5).id();
    let theft = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Confiscate",
            false,
            "Enchant permanent\nYou control enchanted permanent.",
        )
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(complete_publication_cost("{4}{U}{U}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    let mut runner = scenario.build();
    engine::game::effects::attach::attach_to(runner.state_mut(), equipment, old_host);
    assert_eq!(effective_pt(&mut runner, old_host), (4, 5));
    assert!(runner.state().objects[&old_host]
        .keywords
        .contains(&Keyword::Vigilance));
    complete_publication_fund(&mut runner, P0, 4, &[ManaType::Blue, ManaType::Blue]);
    runner.cast(theft).target_object(equipment).resolve();
    assert_eq!(runner.state().objects[&equipment].owner, P1);
    assert_eq!(runner.state().objects[&equipment].controller, P0);
    assert_eq!(runner.state().objects[&old_host].controller, P1);
    assert_eq!(effective_pt(&mut runner, old_host), (4, 5));
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 3, &[]);
    runner
        .act(GameAction::Equip {
            equipment_id: equipment,
            target_id: own_host,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(effective_pt(&mut runner, own_host), (6, 7));
    assert!(runner.state().objects[&own_host]
        .keywords
        .contains(&Keyword::Vigilance));
    assert_eq!(effective_pt(&mut runner, old_host), (2, 3));
    assert!(!runner.state().objects[&old_host]
        .keywords
        .contains(&Keyword::Vigilance));
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(theft).resolve();
    assert_eq!(runner.state().objects[&equipment].controller, P1);
    assert_eq!(runner.state().objects[&own_host].controller, P0);
    assert_eq!(effective_pt(&mut runner, own_host), (6, 7));
    assert!(runner.state().objects[&own_host]
        .keywords
        .contains(&Keyword::Vigilance));
    complete_publication_fund(&mut runner, P0, 3, &[]);
    assert!(
        runner
            .act(GameAction::Equip {
                equipment_id: equipment,
                target_id: own_host
            })
            .is_err(),
        "host controller cannot activate another player's Equipment"
    );
    assert_continuous_complete(runner.state());
}
#[test]
fn complete_continuous_qualified_counts_follow_control_then_source_departure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jodah = scenario
        .add_creature_from_oracle(P0, "Jodah, the Unifier", 5, 5, JODAH)
        .as_legendary()
        .with_subtypes(vec!["Human", "Wizard"])
        .with_mana_cost(complete_publication_cost("{W}{U}{B}{R}{G}"))
        .id();
    let hancock = scenario
        .add_creature_from_oracle(P0, "Hancock, Ghoulish Mayor", 2, 1, HANCOCK)
        .as_legendary()
        .with_subtypes(vec!["Zombie", "Mutant", "Advisor"])
        .with_mana_cost(complete_publication_cost("{2}{B}"))
        .id();
    scenario.with_counter(hancock, CounterType::Generic("charge".into()), 2);
    scenario.with_counter(hancock, CounterType::Generic("blood".into()), 1);
    let own = scenario
        .add_creature(P0, "Own Zombie", 2, 3)
        .with_subtypes(vec!["Zombie"])
        .id();
    let foreign = scenario
        .add_creature(P1, "Foreign Mutant", 5, 7)
        .with_subtypes(vec!["Mutant"])
        .id();
    let theft=scenario.add_spell_to_hand_from_oracle(P1,"Turn Against",true,
        "Devoid (This card has no color.)\nGain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.")
        .with_mana_cost(complete_publication_cost("{4}{R}")).id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    for player in [P0, P1] {
        for _ in 0..4 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    assert_eq!(effective_pt(&mut runner, jodah), (7, 7));
    assert_eq!(effective_pt(&mut runner, own), (5, 6));
    let epoch = runner.state().objects[&hancock].incarnation;
    complete_publication_fund(&mut runner, P1, 4, &[ManaType::Red]);
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(theft).target_object(hancock).resolve();
    assert_eq!(runner.state().objects[&hancock].owner, P0);
    assert_eq!(runner.state().objects[&hancock].controller, P1);
    assert_eq!(runner.state().objects[&hancock].incarnation, epoch);
    assert_eq!(effective_pt(&mut runner, jodah), (6, 6));
    assert_eq!(effective_pt(&mut runner, hancock), (2, 1));
    assert_eq!(effective_pt(&mut runner, own), (2, 3));
    assert_eq!(effective_pt(&mut runner, foreign), (8, 10));
    assert_continuous_complete(runner.state());
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    assert_eq!(runner.state().objects[&hancock].controller, P0);
    assert_eq!(effective_pt(&mut runner, jodah), (7, 7));
    assert_eq!(effective_pt(&mut runner, own), (5, 6));
    assert_eq!(effective_pt(&mut runner, foreign), (5, 7));
    assert_continuous_complete(runner.state());
    runner.advance_to_phase(Phase::PreCombatMain);
    if runner.state().priority_player != P0 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(hancock).resolve();
    assert_eq!(runner.state().objects[&hancock].zone, Zone::Hand);
    assert_eq!(effective_pt(&mut runner, jodah), (6, 6));
    assert_eq!(effective_pt(&mut runner, own), (2, 3));
    assert_eq!(effective_pt(&mut runner, foreign), (5, 7));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_drop_bear_zero_and_owner_former_controller_departure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_hand_from_oracle(P0, "Drop Bear", 0, 0, DROP_BEAR)
        .with_subtypes(vec!["Koala", "Bear", "Horror"])
        .with_mana_cost(complete_publication_cost("{3}{R}{G}"))
        .id();
    let forest = scenario
        .add_land_to_hand(P0, "Forest")
        .with_subtypes(vec!["Forest"])
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let foreign_bear = scenario
        .add_creature(P1, "Foreign Bear", 2, 4)
        .with_subtypes(vec!["Bear"])
        .id();
    let theft=scenario.add_spell_to_hand_from_oracle(P1,"Turn Against",true,
        "Devoid (This card has no color.)\nGain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.")
        .with_mana_cost(complete_publication_cost("{4}{R}")).id();
    let swords = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Swords to Plowshares",
            true,
            "Exile target creature. Its controller gains life equal to its power.",
        )
        .with_mana_cost(complete_publication_cost("{W}"))
        .id();
    let mut runner = scenario.build();
    assert_eq!(effective_pt(&mut runner, source), (0, 0));
    assert_eq!(runner.state().objects[&source].layer_base_power, Some(0));
    assert_eq!(
        runner.state().objects[&source].zone,
        Zone::Hand,
        "known off-zone zero is not a battlefield SBA death"
    );
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id: runner.state().objects[&forest].card_id,
        })
        .unwrap();
    assert_eq!(effective_pt(&mut runner, source), (1, 1));
    complete_publication_fund(&mut runner, P0, 3, &[ManaType::Red, ManaType::Green]);
    runner.cast(source).commit();
    runner.pass_both_players();
    let WaitingFor::TriggerTargetSelection {
        player,
        source_id,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual Bear entry choice")
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, Some(source));
    assert_eq!(target_slots.len(), 1);
    assert!(target_slots[selection.current_slot].optional);
    assert!(target_slots[selection.current_slot]
        .legal_targets
        .contains(&TargetRef::Object(foreign_bear)));
    runner
        .act(GameAction::ChooseTarget { target: None })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(effective_pt(&mut runner, source), (2, 2));
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P1, 4, &[ManaType::Red]);
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(theft).target_object(source).resolve();
    assert_eq!(runner.state().objects[&source].owner, P0);
    assert_eq!(runner.state().objects[&source].controller, P1);
    assert_eq!(effective_pt(&mut runner, source), (5, 5));
    let life = runner.state().players[1].life;
    complete_publication_fund(&mut runner, P0, 0, &[ManaType::White]);
    if runner.state().priority_player != P0 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    runner.cast(swords).target_object(source).resolve();
    assert_eq!(
        runner.state().players[1].life,
        life + 5,
        "departure controller gains last battlefield power"
    );
    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert_eq!(
        effective_pt(&mut runner, source),
        (1, 1),
        "current exile value uses its owner population"
    );
    assert_eq!(runner.state().objects[&source].layer_base_power, Some(1));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_drop_bear_phasing_preserves_epoch_and_live_populations() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Green);
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let first = scenario
        .add_creature_from_oracle(P0, "Drop Bear", 0, 0, DROP_BEAR)
        .with_subtypes(vec!["Koala", "Bear", "Horror"])
        .with_mana_cost(complete_publication_cost("{3}{R}{G}"))
        .id();
    let second = scenario
        .add_creature_from_oracle(P0, "Drop Bear", 0, 0, DROP_BEAR)
        .with_subtypes(vec!["Koala", "Bear", "Horror"])
        .with_mana_cost(complete_publication_cost("{3}{R}{G}"))
        .id();
    let foreign = scenario
        .add_creature_from_oracle(P1, "Drop Bear", 0, 0, DROP_BEAR)
        .with_subtypes(vec!["Koala", "Bear", "Horror"])
        .with_mana_cost(complete_publication_cost("{3}{R}{G}"))
        .id();
    let ripple=scenario.add_spell_to_hand_from_oracle(P0,"Reality Ripple",true,
        "Target artifact, creature, or land phases out. (While it's phased out, it's treated as though it doesn't exist. It phases in before its controller untaps during their next untap step.)")
        .with_mana_cost(complete_publication_cost("{1}{U}")).id();
    for player in [P0, P1] {
        for _ in 0..6 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    assert_eq!(effective_pt(&mut runner, first), (3, 3));
    assert_eq!(effective_pt(&mut runner, second), (3, 3));
    assert_eq!(effective_pt(&mut runner, foreign), (4, 4));
    let epoch = runner.state().objects[&first].incarnation;
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(ripple).target_object(first).resolve();
    // CR 702.26b + CR 702.26d: phased-out Bears leave the count without a
    // new incarnation or a zone-change/entry event.
    assert!(runner.state().objects[&first].is_phased_out());
    assert_eq!(runner.state().objects[&first].incarnation, epoch);
    assert_eq!(effective_pt(&mut runner, second), (2, 2));
    assert_eq!(effective_pt(&mut runner, foreign), (4, 4));
    assert_continuous_complete(runner.state());
    for active in [P0, P1] {
        assert_eq!(runner.state().active_player, active);
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.pass_both_players();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_continuous_complete(runner.state());
    }
    assert_eq!(runner.state().active_player, P0);
    assert!(!runner.state().objects[&first].is_phased_out());
    assert_eq!(runner.state().objects[&first].incarnation, epoch);
    assert_eq!(effective_pt(&mut runner, first), (3, 3));
    assert_eq!(effective_pt(&mut runner, second), (3, 3));
    assert_eq!(effective_pt(&mut runner, foreign), (4, 4));
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_history_full_food_draft_reflexive_and_restricted_mana() {
    use engine::game::combat::AttackTarget;
    use engine::types::mana::ManaRestriction;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Oyaminartok, Polar Werebear", 4, 5, OYAMINARTOK)
        .as_legendary()
        .with_subtypes(vec!["Giant", "Bear"])
        .with_mana_cost(complete_publication_cost("{2}{G}{G}"))
        .id();
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
        .with_color(vec![ManaColor::Blue])
        .with_mana_cost(complete_publication_cost("{2}{U}"))
        .id();
    let atlas = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Walking Atlas",
            1,
            1,
            "{T}: You may put a land card from your hand onto the battlefield.",
        )
        .as_artifact()
        .as_creature()
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(complete_publication_cost("{2}"))
        .id();
    for player in [P0, P1] {
        for _ in 0..4 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    // Initial printed data: the complete pinned relatedCards.spellbook list.
    let spellbook = [
        "Archipelagore",
        "Junk Winder",
        "Moat Piranhas",
        "Mystic Skyfish",
        "Nadir Kraken",
        "Nezahal, Primal Tide",
        "Pouncing Shoreshark",
        "Pursued Whale",
        "Riptide Turtle",
        "Ruin Crab",
        "Sea-Dasher Octopus",
        "Sigiled Starfish",
        "Spined Megalodon",
        "Stinging Lionfish",
        "Voracious Greatshark",
    ];
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .spellbook = spellbook.iter().map(|name| (*name).into()).collect();
    let printed: engine::database::mtgjson::AtomicCard = serde_json::from_value(serde_json::json!({
        "name":"Moat Piranhas", "text":"Defender (This creature can't attack.)", "manaCost":"{1}{U}",
        "manaValue":2.0, "power":"3", "toughness":"3", "types":["Creature"], "subtypes":["Fish"],
        "supertypes":[], "type":"Creature — Fish", "colors":["U"], "colorIdentity":["U"],
        "keywords":["Defender"], "layout":"normal", "identifiers":{}
    })).unwrap();
    let face = engine::database::synthesis::build_oracle_face(&printed, None);
    std::sync::Arc::make_mut(&mut runner.state_mut().card_face_registry)
        .insert("moat piranhas".into(), face);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .unwrap();
    runner.advance_to_phase(Phase::DeclareBlockers);
    runner.declare_blockers(&[]).unwrap();
    runner.advance_to_phase(Phase::PostCombatMain);
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects[&source]
        .keywords
        .contains(&Keyword::Hexproof));
    let food = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner.state().objects[id].is_token
                && runner.state().objects[id]
                    .card_types
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Food")
        })
        .collect::<Vec<_>>();
    assert_eq!(food.len(), 1);
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 0, &[ManaType::Blue]);
    runner
        .act(GameAction::ActivateAbility {
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual Food sacrifice cost")
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(choices, &food);
    runner
        .act(GameAction::SelectCards {
            cards: food.clone(),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    let WaitingFor::SpellbookDraft {
        player,
        source_id,
        options,
        destination,
        tapped,
    } = &runner.state().waiting_for
    else {
        panic!("full printed draft body")
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, source);
    assert_eq!(*destination, Zone::Hand);
    assert!(!tapped);
    assert_eq!(
        options,
        &spellbook
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    );
    assert!(runner
        .act(GameAction::SubmitSpellbookDraft {
            card: "Divination".into()
        })
        .is_err());
    runner
        .act(GameAction::SubmitSpellbookDraft {
            card: "Moat Piranhas".into(),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    // CR 704.5d: the sacrificed Food token ceases to exist at the next SBA check.
    assert!(!runner.state().objects.contains_key(&food[0]));
    let drafted = runner.state().players[0]
        .hand
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Moat Piranhas")
        .unwrap();
    assert_eq!(effective_pt(&mut runner, drafted), (3, 3));
    assert!(runner.state().objects[&drafted]
        .keywords
        .contains(&Keyword::Defender));
    assert_eq!(runner.state().players[0].mana_pool.total(), 3);
    for unit in runner.state().players[0].mana_pool.units() {
        assert_eq!(unit.color, ManaType::Blue);
        assert_eq!(unit.source_id, source);
        assert!(unit
            .restrictions
            .contains(&ManaRestriction::OnlyForSpellType("Creature".into())));
        assert!(unit
            .restrictions
            .contains(&ManaRestriction::OnlyForSpellColor(ManaColor::Blue)));
    }
    assert_continuous_complete(runner.state());
    assert!(
        runner.cast(divination).try_resolve().is_err(),
        "three blue mana cannot pay for a blue noncreature spell"
    );
    assert!(
        runner.cast(atlas).try_resolve().is_err(),
        "three blue mana cannot pay for a colorless creature spell"
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 3);
    runner.cast(drafted).resolve();
    assert_eq!(runner.state().objects[&drafted].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[0].mana_pool.total(), 1);
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_rakdos_pending_grant_keeps_equipment_after_detach_or_control_change() {
    use engine::types::game_state::CastPaymentMode;
    for steal in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let knife = scenario
            .add_artifact_from_oracle(P0, "Rakdos Riteknife", RITEKNIFE)
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(complete_publication_cost("{2}"))
            .id();
        let host = scenario
            .add_creature(P0, "Original granted host", 2, 3)
            .id();
        let fodder = scenario.add_creature(P0, "Sacrifice choice", 1, 1).id();
        let next = scenario
            .add_creature(P1, "Foreign replacement host", 5, 6)
            .id();
        let actor = if steal { P1 } else { P0 };
        let response = if steal {
            scenario.add_spell_to_hand_from_oracle(actor, "Word of Seizing", true,
                "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and gain control of it until end of turn. It gains haste until end of turn.")
                .with_mana_cost(complete_publication_cost("{3}{R}{R}")).id()
        } else {
            scenario.add_spell_to_hand_from_oracle(actor, "Magnetic Theft", true,
                "Attach target Equipment to target creature. (Control of the Equipment doesn't change.)")
                .with_mana_cost(complete_publication_cost("{R}")).id()
        };
        let mut runner = scenario.build();
        complete_publication_fund(&mut runner, P0, 2, &[]);
        runner
            .act(GameAction::Equip {
                equipment_id: knife,
                target_id: host,
            })
            .unwrap();
        runner.advance_until_stack_empty();
        let epoch = runner.state().objects[&knife].incarnation;
        let index = runner.state().objects[&host]
            .abilities
            .iter()
            .position(|definition| {
                definition
                    .granting_object
                    .is_some_and(|reference| reference.object_id == knife)
            })
            .unwrap();
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: host,
                ability_index: index,
            })
            .unwrap();
        let WaitingFor::PayCost {
            player,
            choices,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("granted sacrifice")
        };
        assert_eq!(*player, P0);
        assert_eq!(*count, 1);
        assert!(choices.contains(&host));
        assert!(choices.contains(&fodder));
        runner
            .act(GameAction::SelectCards {
                cards: vec![fodder],
            })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        if actor == P1 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        complete_publication_fund(
            &mut runner,
            actor,
            if steal { 3 } else { 0 },
            if steal {
                &[ManaType::Red, ManaType::Red]
            } else {
                &[ManaType::Red]
            },
        );
        runner
            .act(GameAction::CastSpell {
                object_id: response,
                card_id: runner.state().objects[&response].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::default(),
            })
            .unwrap();
        let expected = if steal {
            vec![knife]
        } else {
            vec![knife, next]
        };
        for (slot_index, chosen) in expected.iter().enumerate() {
            let WaitingFor::TargetSelection {
                player,
                target_slots,
                selection,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("real response announcement")
            };
            assert_eq!(*player, actor);
            assert_eq!(target_slots.len(), expected.len());
            assert_eq!(selection.current_slot, slot_index);
            let slot = &target_slots[selection.current_slot];
            assert!(!slot.optional);
            assert!(slot.legal_targets.contains(&TargetRef::Object(*chosen)));
            if !steal && slot_index == 0 {
                assert!(!slot.legal_targets.contains(&TargetRef::Object(host)));
            }
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(*chosen)),
                })
                .unwrap();
        }
        assert_eq!(runner.state().stack.len(), 2);
        runner.resolve_top();
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack[0].ability().unwrap().source_id, host);
        assert_eq!(runner.state().objects[&knife].incarnation, epoch);
        assert_eq!(runner.state().objects[&knife].owner, P0);
        assert_eq!(
            runner.state().objects[&knife].controller,
            if steal { P1 } else { P0 }
        );
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&knife]
                .counters
                .get(&CounterType::Generic("blood".into())),
            Some(&1)
        );
        assert_eq!(
            effective_pt(&mut runner, host),
            if steal { (3, 3) } else { (2, 3) }
        );
        assert_eq!(
            effective_pt(&mut runner, next),
            if steal { (5, 6) } else { (6, 6) }
        );
        assert_eq!(runner.state().objects[&host].controller, P0);
        assert_eq!(runner.state().objects[&next].controller, P1);
        let recipient = if steal { host } else { next };
        assert!(runner.state().objects[&recipient]
            .abilities
            .iter()
            .any(|definition| definition
                .granting_object
                .is_some_and(|reference| reference.object_id == knife)));
        assert_continuous_complete(runner.state());
    }
}

// SHAPE: this constructed conditional grammar control complements the five
// complete real Archetype sources above. Turn changes drive its live predicate.
#[test]
fn complete_continuous_hosted_member_condition_and_denial_are_live_each_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_spell_to_hand_from_oracle(P0, "Conditional denial grammar control", false,
        "During your turn, creatures your opponents control lose flying and can't have or gain flying.")
        .as_enchantment().with_mana_cost(complete_publication_cost("{1}{W}")).id();
    let own = scenario
        .add_creature(P0, "Own flying guard", 2, 3)
        .with_keyword(Keyword::Flying)
        .id();
    let foreign = scenario
        .add_creature(P1, "Opposing printed flying", 3, 4)
        .with_keyword(Keyword::Flying)
        .id();
    let later = scenario
        .add_creature(P1, "Opposing later recipient", 4, 5)
        .id();
    let grant = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Levitation",
            false,
            "Creatures you control have flying.",
        )
        .as_enchantment()
        .with_mana_cost(complete_publication_cost("{2}{U}{U}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    for player in [P0, P1] {
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::White]);
    runner.cast(source).resolve();
    assert!(runner.state().objects[&own]
        .keywords
        .contains(&Keyword::Flying));
    assert!(!runner.state().objects[&foreign]
        .keywords
        .contains(&Keyword::Flying));
    assert_continuous_complete(runner.state());
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    effective_pt(&mut runner, foreign);
    assert!(runner.state().objects[&foreign]
        .keywords
        .contains(&Keyword::Flying));
    complete_publication_fund(&mut runner, P1, 2, &[ManaType::Blue, ManaType::Blue]);
    runner.cast(grant).resolve();
    assert!(runner.state().objects[&later]
        .keywords
        .contains(&Keyword::Flying));
    assert_continuous_complete(runner.state());
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    effective_pt(&mut runner, foreign);
    assert!(runner.state().objects[&own]
        .keywords
        .contains(&Keyword::Flying));
    for object in [foreign, later] {
        assert!(!runner.state().objects[&object]
            .keywords
            .contains(&Keyword::Flying));
    }
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(source).resolve();
    for object in [foreign, later] {
        assert!(runner.state().objects[&object]
            .keywords
            .contains(&Keyword::Flying));
    }
    assert_continuous_complete(runner.state());
}

#[test]
fn complete_continuous_zinnia_counts_follow_recipient_and_source_controllers_until_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Zinnia, Valley's Voice", 1, 3, ZINNIA)
        .as_legendary()
        .with_subtypes(vec!["Bird", "Bard"])
        .with_mana_cost(complete_publication_cost("{U}{R}{W}"))
        .id();
    let own = scenario
        .add_creature(P0, "Own base-one with counters", 1, 4)
        .with_plus_counters(2)
        .id();
    let another = scenario
        .add_creature(P0, "Other base-one recipient", 1, 5)
        .id();
    let excluded = scenario
        .add_creature(P0, "Current-one base-two control", 2, 4)
        .with_minus_counters(1)
        .id();
    let foreign = scenario.add_creature(P1, "Foreign base-one", 1, 6).id();
    let take_recipient = scenario.add_spell_to_hand_from_oracle(P0, "Turn Against", true,
        "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.")
        .with_mana_cost(complete_publication_cost("{4}{R}")).id();
    let take_source = scenario.add_spell_to_hand_from_oracle(P1, "Turn Against", true,
        "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.")
        .with_mana_cost(complete_publication_cost("{4}{R}")).id();
    let mut runner = scenario.build();
    assert_eq!(effective_pt(&mut runner, source), (3, 3));
    assert_eq!(effective_pt(&mut runner, own), (3, 6));
    assert_eq!(effective_pt(&mut runner, excluded), (1, 3));
    complete_publication_fund(&mut runner, P0, 4, &[ManaType::Red]);
    runner.cast(take_recipient).target_object(foreign).resolve();
    assert_eq!(runner.state().objects[&foreign].owner, P1);
    assert_eq!(runner.state().objects[&foreign].controller, P0);
    assert_eq!(effective_pt(&mut runner, source), (4, 3));
    assert_continuous_complete(runner.state());
    let epoch = runner.state().objects[&source].incarnation;
    runner.act(GameAction::PassPriority).unwrap();
    complete_publication_fund(&mut runner, P1, 4, &[ManaType::Red]);
    runner.cast(take_source).target_object(source).resolve();
    assert_eq!(runner.state().objects[&source].owner, P0);
    assert_eq!(runner.state().objects[&source].controller, P1);
    assert_eq!(runner.state().objects[&source].incarnation, epoch);
    assert_eq!(
        effective_pt(&mut runner, source),
        (1, 3),
        "current controller has no OTHER base-one creatures"
    );
    assert_eq!(effective_pt(&mut runner, another), (1, 5));
    assert!(runner.state().objects[&source]
        .keywords
        .contains(&Keyword::Flying));
    assert_continuous_complete(runner.state());
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.pass_both_players();
    assert_eq!(runner.state().objects[&source].controller, P0);
    assert_eq!(runner.state().objects[&foreign].controller, P1);
    assert_eq!(effective_pt(&mut runner, source), (3, 3));
    assert_continuous_complete(runner.state());
}

// Complete pinned MTGJSON English fixtures (2026-10-04); same face supplies
// both runtime definitions and the retained structured diagnostics.
fn duration_repair_face(name: &str) -> engine::types::card::CardFace {
    let card: engine::database::mtgjson::AtomicCard = serde_json::from_str(match name {
        "Chong and Lily, Nomads" => r###"{"name":"Chong and Lily, Nomads","text":"Whenever one or more Bards you control attack, choose one —\n• Put a lore counter on each of any number of target Sagas you control.\n• Creatures you control get +1/+0 until end of turn for each lore counter among Sagas you control.","manaCost":"{3}{R}","manaValue":4.0,"types":["Creature"],"type":"Legendary Creature — Human Bard Ally","subtypes":["Human","Bard","Ally"],"supertypes":["Legendary"],"power":"3","toughness":"3","colors":["R"],"colorIdentity":["R"],"layout":"normal","identifiers":{"scryfallOracleId":"faffc9fb-32b3-4015-8c3f-e6b45002c050"}}"###,
        "Phyresis Outbreak" => r###"{"name":"Phyresis Outbreak","text":"Each opponent gets a poison counter. Then each creature your opponents control gets -1/-1 until end of turn for each poison counter its controller has.","manaCost":"{2}{B}","manaValue":3.0,"types":["Sorcery"],"type":"Sorcery","subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","identifiers":{"scryfallOracleId":"47148b5d-3850-4614-822e-6ef2755996f8"}}"###,
        "Calming Licid" => r###"{"name":"Calming Licid","text":"{W}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {W} to end this effect.\nEnchanted creature can't attack.","manaCost":"{2}{W}","manaValue":3.0,"types":["Creature"],"type":"Creature — Licid","subtypes":["Licid"],"supertypes":[],"power":"2","toughness":"2","colors":["W"],"colorIdentity":["W"],"layout":"normal","identifiers":{"scryfallOracleId":"93d20859-249f-47ae-8f6d-095228e7b6e9"}}"###,
        "Thunderous Orator" => r###"{"name":"Thunderous Orator","text":"Vigilance\nWhenever this creature attacks, it gains flying until end of turn if you control a creature with flying. The same is true for first strike, double strike, deathtouch, indestructible, lifelink, menace, and trample.","manaCost":"{1}{W}","manaValue":2.0,"types":["Creature"],"type":"Creature — Kor Wizard","subtypes":["Kor","Wizard"],"supertypes":[],"power":"2","toughness":"2","colors":["W"],"colorIdentity":["W"],"keywords":["Vigilance"],"layout":"normal","identifiers":{"scryfallOracleId":"7716ad72-9e96-4915-bc1e-e12b4d0f6f9c"}}"###,
        "Odric, Lunarch Marshal" => r###"{"name":"Odric, Lunarch Marshal","text":"At the beginning of each combat, creatures you control gain first strike until end of turn if a creature you control has first strike. The same is true for flying, deathtouch, double strike, haste, hexproof, indestructible, lifelink, menace, reach, skulk, trample, and vigilance.","manaCost":"{3}{W}","manaValue":4.0,"types":["Creature"],"type":"Legendary Creature — Human Soldier","subtypes":["Human","Soldier"],"supertypes":["Legendary"],"power":"3","toughness":"3","colors":["W"],"colorIdentity":["W"],"layout":"normal","identifiers":{"scryfallOracleId":"bad76170-c773-4be5-9457-20dc9f745cb4"}}"###,
        "Bleeding Effect" => r###"{"name":"Bleeding Effect","text":"At the beginning of combat on your turn, creatures you control gain flying until end of turn if a creature card in your graveyard has flying. The same is true for first strike, double strike, deathtouch, hexproof, indestructible, lifelink, menace, reach, trample, and vigilance.","manaCost":"{2}{W}{B}","manaValue":4.0,"types":["Enchantment"],"type":"Enchantment","subtypes":[],"supertypes":[],"colors":["B","W"],"colorIdentity":["B","W"],"layout":"normal","identifiers":{"scryfallOracleId":"689c9a49-d0f5-494a-9fa2-948cd5517e74"}}"###,
        "Dread Charge" => r###"{"name":"Dread Charge","text":"Black creatures you control can't be blocked this turn except by black creatures.","manaCost":"{3}{B}","manaValue":4.0,"types":["Sorcery"],"type":"Sorcery","subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","identifiers":{"scryfallOracleId":"6b7933b3-821a-4d64-9a10-af8a69fa008e"}}"###,
        "Tin Street Dodger" => r###"{"name":"Tin Street Dodger","text":"Haste\n{R}: This creature can't be blocked this turn except by creatures with defender.","manaCost":"{R}","manaValue":1.0,"types":["Creature"],"type":"Creature — Goblin Rogue","subtypes":["Goblin","Rogue"],"supertypes":[],"power":"1","toughness":"1","colors":["R"],"colorIdentity":["R"],"keywords":["Haste"],"layout":"normal","identifiers":{"scryfallOracleId":"fde3c8df-cc57-4e64-b78b-301e103d8487"}}"###,
        "Departed Deckhand" => r###"{"name":"Departed Deckhand","text":"When this creature becomes the target of a spell, sacrifice it.\nThis creature can't be blocked except by Spirits.\n{3}{U}: Another target creature you control can't be blocked this turn except by Spirits.","manaCost":"{1}{U}","manaValue":2.0,"types":["Creature"],"type":"Creature — Spirit Pirate","subtypes":["Spirit","Pirate"],"supertypes":[],"power":"2","toughness":"2","colors":["U"],"colorIdentity":["U"],"layout":"normal","identifiers":{"scryfallOracleId":"a620a765-97ba-4687-acfd-4dec7da75d9f"}}"###,
        "Sensei Golden-Tail" => r###"{"name":"Sensei Golden-Tail","text":"Bushido 1 (Whenever this creature blocks or becomes blocked, it gets +1/+1 until end of turn.)\n{1}{W}, {T}: Put a training counter on target creature. That creature gains bushido 1 and becomes a Samurai in addition to its other creature types. Activate only as a sorcery.","manaCost":"{1}{W}","manaValue":2.0,"types":["Creature"],"type":"Legendary Creature — Fox Samurai","subtypes":["Fox","Samurai"],"supertypes":["Legendary"],"power":"2","toughness":"1","colors":["W"],"colorIdentity":["W"],"keywords":["Bushido"],"layout":"normal","identifiers":{"scryfallOracleId":"63305fd4-068f-498b-a01e-0b375c592797"}}"###,
        "Grizzly Bears" => r###"{"name":"Grizzly Bears","manaCost":"{1}{G}","manaValue":2.0,"types":["Creature"],"type":"Creature — Bear","subtypes":["Bear"],"supertypes":[],"power":"2","toughness":"2","colors":["G"],"colorIdentity":["G"],"layout":"normal","identifiers":{"scryfallOracleId":"14c8f55d-d177-4c25-a931-ebeb9e6062a0"}}"###,
        "Soulmender" => r###"{"name":"Soulmender","text":"{T}: You gain 1 life.","manaCost":"{W}","manaValue":1.0,"types":["Creature"],"type":"Creature — Human Cleric","subtypes":["Human","Cleric"],"supertypes":[],"power":"1","toughness":"1","colors":["W"],"colorIdentity":["W"],"layout":"normal","identifiers":{"scryfallOracleId":"41b1d850-2fda-490b-a4f6-0d9bfb37103c"}}"###,
        "Mutilate" => r###"{"name":"Mutilate","text":"All creatures get -1/-1 until end of turn for each Swamp you control.","manaCost":"{2}{B}{B}","manaValue":4.0,"types":["Sorcery"],"type":"Sorcery","subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","identifiers":{"scryfallOracleId":"698e79ec-c2bc-4973-a095-f3c57b50e39c"}}"###,
        _ => panic!("unlisted pinned duration fixture: {name}"),
    }).unwrap();
    let face = engine::database::synthesis::build_oracle_face(
        &card,
        card.identifiers.scryfall_oracle_id.clone(),
    );
    assert_eq!(face.oracle_text, card.text);
    assert_eq!(face.card_type.subtypes, card.subtypes);
    face
}

fn duration_repair_install_face(
    runner: &mut GameRunner,
    id: ObjectId,
    face: &engine::types::card::CardFace,
) {
    engine::game::printed_cards::apply_card_face_to_object(
        runner.state_mut().objects.get_mut(&id).unwrap(),
        face,
    );
    std::sync::Arc::make_mut(&mut runner.state_mut().card_face_registry)
        .insert(face.name.to_lowercase(), face.clone());
    runner.state_mut().layers_dirty.mark_full();
    if runner.state().objects[&id].zone == Zone::Battlefield {
        engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), id);
    }
}

fn duration_repair_assert_quantity_gap(face: &engine::types::card::CardFace, operand: &str) {
    let value = serde_json::to_value(face).unwrap();
    let (path, controller) = match face.name.as_str() {
        "Chong and Lily, Nomads" => ("/triggers/0/execute/mode_abilities/1", "You"),
        "Phyresis Outbreak" => ("/abilities/0/sub_ability", "Opponent"),
        _ => panic!("bounded unsupported-quantity witnesses only"),
    };
    let node = value.pointer(path).unwrap();
    assert_eq!(node["effect"]["type"], "PumpAll");
    assert_eq!(
        node["effect"]["target"],
        serde_json::json!({"type":"Typed","type_filters":["Creature"],"controller":controller,"properties":[]})
    );
    assert_eq!(node["duration"], "UntilEndOfTurn");
    assert!(node.get("multi_target").is_none());
    let warning = value["parse_warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["type"] == "SwallowedClause" && w["detector"] == "DynamicQty")
        .unwrap();
    assert_eq!(warning["line_index"], 0);
    assert_eq!(warning["items"], serde_json::json!([0]));
    assert_eq!(
        warning["gap"],
        serde_json::json!({"kind":"unparsed_quantity","operand":operand})
    );
    assert_eq!(
        warning["unit_span"],
        serde_json::json!({"first_line":0,"last_line":if face.name == "Chong and Lily, Nomads" {2} else {0},"start_byte":0,"end_byte":face.oracle_text.as_ref().unwrap().len(),"precision":"Exact","ordinal_within_span":0})
    );
}

#[test]
fn duration_repair_chong_real_attack_second_mode_affects_each_own_creature_and_keeps_quantity_red()
{
    let face = duration_repair_face("Chong and Lily, Nomads");
    duration_repair_assert_quantity_gap(&face, "lore counter among sagas you control");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, &face.name, 3, 3).id();
    let own = scenario
        .add_creature(P1, "Own population witness", 2, 7)
        .controlled_by(P0)
        .id();
    let foreign = scenario
        .add_creature(P0, "Opposing creature control", 4, 8)
        .controlled_by(P1)
        .id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Own noncreature control", "")
        .id();
    let saga_a = scenario
        .add_enchantment_from_oracle(P0, "Saga with two lore", "")
        .with_subtypes(vec!["Saga"])
        .id();
    let saga_b = scenario
        .add_enchantment_from_oracle(P0, "Saga with three lore", "")
        .with_subtypes(vec!["Saga"])
        .id();
    scenario
        .with_counter(saga_a, CounterType::Lore, 2)
        .with_counter(saga_b, CounterType::Lore, 3);
    let late = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .with_mana_cost(complete_publication_cost("{1}{G}"))
        .id();
    let blink = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true, "Exile target creature you control, then return that card to the battlefield under your control.").with_mana_cost(complete_publication_cost("{W}")).id();
    for player in [P0, P1] {
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    duration_repair_install_face(&mut runner, source, &face);
    duration_repair_install_face(&mut runner, late, &duration_repair_face("Grizzly Bears"));
    assert_eq!(
        runner.state().objects[&saga_a]
            .counters
            .get(&CounterType::Lore),
        Some(&2)
    );
    assert_eq!(
        runner.state().objects[&saga_b]
            .counters
            .get(&CounterType::Lore),
        Some(&3)
    );
    assert_eq!(
        runner.state().objects[&saga_a].counters[&CounterType::Lore]
            + runner.state().objects[&saga_b].counters[&CounterType::Lore],
        5
    );
    let before = [
        (source, effective_pt(&mut runner, source)),
        (own, effective_pt(&mut runner, own)),
        (foreign, effective_pt(&mut runner, foreign)),
    ];
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(source, engine::game::combat::AttackTarget::Player(P1))])
        .unwrap();
    let WaitingFor::AbilityModeChoice {
        player,
        source_id,
        modal,
        mode_abilities,
        unavailable_modes,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "actual Bard attack modal prompt: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!((*player, *source_id), (P0, source));
    assert_eq!(
        (modal.min_choices, modal.max_choices, modal.mode_count),
        (1, 1, 2)
    );
    assert_eq!(mode_abilities.len(), 2);
    assert!(unavailable_modes.is_empty());
    assert_eq!(modal.mode_descriptions, vec!["Put a lore counter on each of any number of target Sagas you control.","Creatures you control get +1/+0 until end of turn for each lore counter among Sagas you control."]);
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .unwrap();
    assert!(!runner.state().stack.is_empty());
    runner.advance_until_stack_empty();
    assert_continuous_complete(runner.state());
    // CR 115.10a + CR 611.2c: the untargeted class fixes all own recipients.
    for (id, (power, toughness)) in before {
        let actual = effective_pt(&mut runner, id);
        if id == foreign {
            assert_eq!(actual, (power, toughness));
        } else {
            assert!(
                actual.0 > power,
                "every own creature must gain power: {id:?}"
            );
            assert_eq!(actual.1, toughness);
        }
    }
    assert_eq!(
        (
            runner.state().objects[&artifact].power,
            runner.state().objects[&artifact].toughness
        ),
        (None, None)
    );
    assert_eq!(
        (
            runner.state().objects[&saga_a].power,
            runner.state().objects[&saga_b].power
        ),
        (None, None)
    );
    runner.advance_to_phase(Phase::DeclareBlockers);
    runner.declare_blockers(&[]).unwrap();
    runner.advance_to_phase(Phase::PostCombatMain);
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Green]);
    runner.cast(late).resolve();
    assert_eq!(effective_pt(&mut runner, late), (2, 2));
    assert!(effective_pt(&mut runner, source).0 > 3);
    let old_birth = runner.state().objects[&own].incarnation;
    complete_publication_fund(&mut runner, P0, 0, &[ManaType::White]);
    runner.cast(blink).target_object(own).resolve();
    // CR 400.7: a returned incarnation does not inherit the earlier snapshot.
    assert_ne!(runner.state().objects[&own].incarnation, old_birth);
    assert_eq!(effective_pt(&mut runner, own), (2, 7));
    assert!(effective_pt(&mut runner, source).0 > 3);
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    // CR 514.2: the original resolving grant ends at cleanup.
    assert_eq!(effective_pt(&mut runner, source), (3, 3));
    assert_eq!(effective_pt(&mut runner, own), (2, 7));
    assert_continuous_complete(runner.state());
    duration_repair_assert_quantity_gap(&face, "lore counter among sagas you control");
}

#[test]
fn duration_repair_phyresis_paid_three_player_cast_affects_all_opposing_members_and_keeps_quantity_red(
) {
    let face = duration_repair_face("Phyresis Outbreak");
    duration_repair_assert_quantity_gap(&face, "poison counter its controller has");
    for empty in [false, true] {
        let mut scenario = GameScenario::new_n_player(3, 9202);
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                &face.name,
                false,
                face.oracle_text.as_deref().unwrap(),
            )
            .with_mana_cost(complete_publication_cost("{2}{B}"))
            .id();
        let own = scenario
            .add_creature(P1, "Own creature excluded", 8, 20)
            .controlled_by(P0)
            .id();
        let artifact = scenario
            .add_artifact_from_oracle(P1, "Opposing noncreature excluded", "")
            .id();
        let mut affected = Vec::new();
        if !empty {
            for (index, player) in [P1, P1, PlayerId(2), PlayerId(2)].into_iter().enumerate() {
                affected.push((
                    scenario
                        .add_creature(
                            if index % 2 == 0 { P0 } else { player },
                            &format!("Distinct opposing member {index}"),
                            10 + index as i32,
                            20 + index as i32,
                        )
                        .controlled_by(player)
                        .id(),
                    (10 + index as i32, 20 + index as i32),
                ));
            }
        }
        let late = scenario
            .add_creature_to_hand_from_oracle(
                P1,
                "Spire Monitor",
                3,
                3,
                "Flash (You may cast this spell any time you could cast an instant.)\nFlying",
            )
            .with_subtypes(vec!["Phyrexian", "Drake"])
            .with_color(vec![ManaColor::Blue])
            .with_mana_cost(complete_publication_cost("{4}{U}"))
            .id();
        let blink=scenario.add_spell_to_hand_from_oracle(P1,"Cloudshift",true,"Exile target creature you control, then return that card to the battlefield under your control.").with_mana_cost(complete_publication_cost("{W}")).id();
        for player in [P0, P1, PlayerId(2)] {
            for _ in 0..5 {
                scenario.add_card_to_library_top(player, "Draw buffer");
            }
        }
        scenario.with_mana_pool(P0, complete_publication_pool(2, &[ManaType::Black]));
        let mut runner = scenario.build();
        duration_repair_install_face(&mut runner, spell, &face);
        runner.state_mut().players[1].poison_counters = 2;
        runner.state_mut().players[2].poison_counters = 4;
        let commit = runner.cast(spell).commit();
        assert!(matches!(
            commit.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(commit.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(commit.state().players[0].mana_pool.total(), 0);
        assert_eq!(commit.state().stack.len(), 1);
        commit.resolve();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        // CR 608.2c: poison instruction precedes the following class pump.
        assert_eq!(
            (
                runner.state().players[1].poison_counters,
                runner.state().players[2].poison_counters
            ),
            (3, 5)
        );
        assert_eq!(runner.state().players[0].poison_counters, 0);
        for &(id, (power, toughness)) in &affected {
            let actual = effective_pt(&mut runner, id);
            assert!(
                actual.0 < power && actual.1 < toughness,
                "each actual opponent member changes: {id:?} {actual:?}"
            );
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        }
        assert_eq!(effective_pt(&mut runner, own), (8, 20));
        assert_eq!(
            (
                runner.state().objects[&artifact].power,
                runner.state().objects[&artifact].toughness
            ),
            (None, None)
        );
        assert_continuous_complete(runner.state());
        runner.act(GameAction::PassPriority).unwrap();
        complete_publication_fund(&mut runner, P1, 4, &[ManaType::Blue]);
        runner.cast(late).resolve();
        assert_eq!(effective_pt(&mut runner, late), (3, 3));
        assert_continuous_complete(runner.state());
        if !empty {
            let (returned, baseline) = affected[0];
            let birth = runner.state().objects[&returned].incarnation;
            runner.act(GameAction::PassPriority).unwrap();
            complete_publication_fund(&mut runner, P1, 0, &[ManaType::White]);
            runner.cast(blink).target_object(returned).resolve();
            assert_ne!(runner.state().objects[&returned].incarnation, birth);
            assert_eq!(effective_pt(&mut runner, returned), baseline);
            assert!(effective_pt(&mut runner, affected[1].0).0 < affected[1].1 .0);
            assert_continuous_complete(runner.state());
        }
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
        for &(id, baseline) in &affected {
            assert_eq!(effective_pt(&mut runner, id), baseline);
        }
        assert_continuous_complete(runner.state());
        duration_repair_assert_quantity_gap(&face, "poison counter its controller has");
    }
}

#[test]
fn duration_repair_calming_licid_paid_activation_survives_cleanup_and_paid_end_restores_creature() {
    let face = duration_repair_face("Calming Licid");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, &face.name, 2, 2).id();
    let host = scenario
        .add_creature(P1, "Opposing attachment host", 4, 8)
        .id();
    let other = scenario
        .add_creature(P1, "Independent legal attacker", 3, 7)
        .id();
    let own = scenario
        .add_creature(P0, "Own alternative attachment host", 2, 6)
        .id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Noncreature illegal host", "")
        .id();
    for player in [P0, P1] {
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    scenario.with_mana_pool(P0, complete_publication_pool(0, &[ManaType::White]));
    let mut runner = scenario.build();
    duration_repair_install_face(&mut runner, source, &face);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: Default::default(),
        })
        .unwrap();
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "printed Licid's actual target announcement: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(*player, P0);
    assert_eq!(target_slots.len(), 1);
    assert!(!target_slots[selection.current_slot].optional);
    for legal in [host, other, own] {
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(legal)));
    }
    assert!(!target_slots[selection.current_slot]
        .legal_targets
        .contains(&TargetRef::Object(artifact)));
    assert!(runner
        .act(GameAction::ChooseTarget { target: None })
        .is_err());
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(artifact))
        })
        .is_err());
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host)),
        })
        .unwrap();
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(runner.state().objects[&source].tapped);
    assert_eq!(runner.state().stack.len(), 1);
    runner.advance_until_stack_empty();
    // CR 303.4: the source, rather than its chosen host, becomes the Aura.
    assert_eq!(
        runner.state().objects[&source].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(host))
    );
    assert!(runner.state().objects[&host].attachments.contains(&source));
    assert!(runner.state().objects[&source]
        .card_types
        .subtypes
        .contains(&"Aura".into()));
    assert!(!runner.state().objects[&source]
        .card_types
        .core_types
        .contains(&engine::types::card_type::CoreType::Creature));
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    // CR 611.2a: the no-ending-duration transformation survives cleanup.
    assert!(runner.state().objects[&source]
        .card_types
        .subtypes
        .contains(&"Aura".into()));
    assert_eq!(
        runner.state().objects[&source].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(host))
    );
    let attackers = engine::game::combat::get_valid_attacker_ids(runner.state());
    assert!(attackers.contains(&other));
    assert!(!attackers.contains(&host));
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 0, &[ManaType::White]);
    let offered =
        engine::game::end_continuous_effect::end_continuous_effect_candidates(runner.state(), P0);
    assert_eq!(offered.len(), 1);
    assert!(
        engine::game::end_continuous_effect::end_continuous_effect_candidates(runner.state(), P1)
            .is_empty()
    );
    assert_eq!(offered[0].cost, complete_publication_cost("{W}"));
    runner.act(GameAction::PassPriority).unwrap();
    // CR 116.2c: pay the source's actual offered special-action group.
    runner
        .act(GameAction::EndContinuousEffect {
            group: offered[0].group,
            source_name: offered[0].source_name.clone(),
            cost: offered[0].cost.clone(),
        })
        .unwrap();
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(runner.state().objects[&source]
        .card_types
        .core_types
        .contains(&engine::types::card_type::CoreType::Creature));
    assert!(!runner.state().objects[&source]
        .card_types
        .subtypes
        .contains(&"Aura".into()));
    assert_eq!(runner.state().objects[&source].attached_to, None);
    assert!(!runner.state().objects[&host].attachments.contains(&source));
    assert!(engine::game::combat::get_valid_attacker_ids(runner.state()).contains(&host));
    assert!(
        engine::game::end_continuous_effect::end_continuous_effect_candidates(runner.state(), P0)
            .is_empty()
    );
    assert_continuous_complete(runner.state());
}

#[test]
fn duration_repair_sensei_training_type_persists_and_bushido_functions_on_a_later_turn() {
    let face = duration_repair_face("Sensei Golden-Tail");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, &face.name, 2, 1).id();
    let trained = scenario.add_creature(P0, "Training recipient", 2, 10).id();
    let other = scenario
        .add_creature(P0, "Untrained same-controller twin", 2, 10)
        .id();
    let attacker = scenario
        .add_creature(P1, "Later-turn attacking creature", 2, 10)
        .id();
    for player in [P0, P1] {
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    scenario.with_mana_pool(P0, complete_publication_pool(1, &[ManaType::White]));
    let mut runner = scenario.build();
    duration_repair_install_face(&mut runner, source, &face);
    runner.activate(source, 0).target_object(trained).resolve();
    assert!(runner.state().objects[&source].tapped);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(
        runner.state().objects[&trained]
            .counters
            .get(&CounterType::Generic("training".into())),
        Some(&1)
    );
    assert!(runner.state().objects[&trained]
        .card_types
        .subtypes
        .contains(&"Samurai".into()));
    assert!(!runner.state().objects[&other]
        .card_types
        .subtypes
        .contains(&"Samurai".into()));
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert!(runner.state().objects[&trained]
        .card_types
        .subtypes
        .contains(&"Samurai".into()));
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P0))])
        .unwrap();
    runner.advance_to_phase(Phase::DeclareBlockers);
    runner.declare_blockers(&[(trained, attacker)]).unwrap();
    assert!(!runner.state().stack.is_empty());
    runner.advance_until_stack_empty();
    // CR 611.2a + CR 702.45a: training persists and Bushido triggers for the real later block.
    assert_eq!(effective_pt(&mut runner, trained), (3, 11));
    assert_eq!(effective_pt(&mut runner, other), (2, 10));
    assert_continuous_complete(runner.state());
}

#[test]
fn duration_repair_paid_exception_restrictions_keep_allowed_blockers_and_end_at_cleanup() {
    for empty in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let face = duration_repair_face("Dread Charge");
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                &face.name,
                false,
                face.oracle_text.as_deref().unwrap(),
            )
            .with_mana_cost(complete_publication_cost("{3}{B}"))
            .id();
        let own_white = scenario
            .add_creature(P0, "Own white control", 3, 9)
            .with_color(vec![ManaColor::White])
            .id();
        let black_blocker = scenario
            .add_creature(P1, "Allowed black blocker", 3, 9)
            .with_color(vec![ManaColor::Black])
            .id();
        let white_blocker = scenario
            .add_creature(P1, "Disallowed white blocker", 3, 9)
            .with_color(vec![ManaColor::White])
            .id();
        let own = if empty {
            Vec::new()
        } else {
            (0..2)
                .map(|index| {
                    scenario
                        .add_creature(P0, &format!("Own black recipient {index}"), 3, 9)
                        .with_color(vec![ManaColor::Black])
                        .id()
                })
                .collect::<Vec<_>>()
        };
        for player in [P0, P1] {
            for _ in 0..5 {
                scenario.add_card_to_library_top(player, "Draw buffer");
            }
        }
        scenario.with_mana_pool(P0, complete_publication_pool(3, &[ManaType::Black]));
        let mut runner = scenario.build();
        duration_repair_install_face(&mut runner, spell, &face);
        runner.cast(spell).resolve();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        // CR 509.1b: the same restriction admits matching and rejects other blockers.
        for &recipient in &own {
            assert!(engine::game::combat::can_block_pair(
                runner.state(),
                black_blocker,
                recipient
            ));
            assert!(!engine::game::combat::can_block_pair(
                runner.state(),
                white_blocker,
                recipient
            ));
        }
        assert!(engine::game::combat::can_block_pair(
            runner.state(),
            white_blocker,
            own_white
        ));
        assert_continuous_complete(runner.state());
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
        // CR 514.2: the turn-scoped grant has expired.
        for &recipient in &own {
            assert!(engine::game::combat::can_block_pair(
                runner.state(),
                white_blocker,
                recipient
            ));
        }
        assert_continuous_complete(runner.state());
    }
    let face = duration_repair_face("Tin Street Dodger");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, &face.name, 1, 1).id();
    let wall = scenario
        .add_creature(P1, "Defender allowed", 1, 9)
        .with_keyword(Keyword::Defender)
        .id();
    let ordinary = scenario
        .add_creature(P1, "Ordinary blocker refused", 1, 9)
        .id();
    scenario.with_mana_pool(P0, complete_publication_pool(0, &[ManaType::Red]));
    let mut runner = scenario.build();
    duration_repair_install_face(&mut runner, source, &face);
    runner.activate(source, 0).resolve();
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(engine::game::combat::can_block_pair(
        runner.state(),
        wall,
        source
    ));
    assert!(!engine::game::combat::can_block_pair(
        runner.state(),
        ordinary,
        source
    ));
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(engine::game::combat::can_block_pair(
        runner.state(),
        ordinary,
        source
    ));
    assert_continuous_complete(runner.state());
}

// Duration-axis grammar fixture: not represented as a printed card's text.
#[test]
fn duration_repair_interior_next_turn_restriction_survives_first_cleanup_then_expires() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Duration-axis restriction fixture",
            false,
            "Target creature can't be blocked until your next turn except by Walls.",
        )
        .id();
    let recipient = scenario.add_creature(P0, "Fixed recipient", 2, 12).id();
    let wall = scenario
        .add_creature(P1, "Wall allowed", 0, 12)
        .with_subtypes(vec!["Wall"])
        .id();
    let ordinary = scenario.add_creature(P1, "Other blocker", 2, 12).id();
    for player in [P0, P1] {
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    runner.cast(source).target_object(recipient).resolve();
    assert!(engine::game::combat::can_block_pair(
        runner.state(),
        wall,
        recipient
    ));
    assert!(!engine::game::combat::can_block_pair(
        runner.state(),
        ordinary,
        recipient
    ));
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert!(engine::game::combat::can_block_pair(
        runner.state(),
        wall,
        recipient
    ));
    assert!(!engine::game::combat::can_block_pair(
        runner.state(),
        ordinary,
        recipient
    ));
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    assert!(engine::game::combat::can_block_pair(
        runner.state(),
        ordinary,
        recipient
    ));
    assert_continuous_complete(runner.state());
}

#[test]
fn duration_repair_orator_real_attacks_latch_later_true_keyword_after_paid_donor_departure() {
    let face = duration_repair_face("Thunderous Orator");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, &face.name, 2, 2).id();
    let second = scenario.add_creature(P0, &face.name, 2, 2).id();
    let donor = scenario
        .add_creature(P0, "First strike donor", 1, 8)
        .with_keyword(Keyword::FirstStrike)
        .id();
    let foreign = scenario
        .add_creature(P1, "Foreign flying donor", 1, 8)
        .with_keyword(Keyword::Flying)
        .id();
    let own = scenario
        .add_creature(P0, "Nonattacking ungranted twin", 2, 8)
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{U}"))
        .id();
    let late = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .with_mana_cost(complete_publication_cost("{1}{G}"))
        .id();
    for player in [P0, P1] {
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    duration_repair_install_face(&mut runner, late, &duration_repair_face("Grizzly Bears"));
    for id in [source, second] {
        duration_repair_install_face(&mut runner, id, &face);
    }
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (source, engine::game::combat::AttackTarget::Player(P1)),
            (second, engine::game::combat::AttackTarget::Player(P1)),
        ])
        .unwrap();
    let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for else {
        panic!("both actual Orator occurrences require their controller's ordering")
    };
    assert_eq!(*player, P0);
    assert_eq!(triggers.len(), 2);
    let mut sources = triggers
        .iter()
        .map(|trigger| trigger.source_id)
        .collect::<Vec<_>>();
    sources.sort();
    let mut expected = vec![source, second];
    expected.sort();
    assert_eq!(sources, expected);
    // CR 603.3b: choose a nonidentity order for the two real occurrences.
    runner
        .act(GameAction::OrderTriggers { order: vec![1, 0] })
        .unwrap();
    runner.advance_until_stack_empty();
    for id in [source, second] {
        assert!(runner.state().objects[&id]
            .keywords
            .contains(&Keyword::FirstStrike));
        assert!(!runner.state().objects[&id]
            .keywords
            .contains(&Keyword::Flying));
    }
    assert!(!runner.state().objects[&own]
        .keywords
        .contains(&Keyword::FirstStrike));
    assert!(!runner.state().objects[&foreign]
        .keywords
        .contains(&Keyword::FirstStrike));
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 0, &[ManaType::Blue]);
    runner.cast(bounce).target_object(donor).resolve();
    assert_eq!(runner.state().objects[&donor].zone, Zone::Hand);
    // CR 611.2c: conditions latch for each resolving occurrence.
    for id in [source, second] {
        assert!(runner.state().objects[&id]
            .keywords
            .contains(&Keyword::FirstStrike));
    }
    assert_continuous_complete(runner.state());
    runner.advance_to_phase(Phase::DeclareBlockers);
    runner.declare_blockers(&[]).unwrap();
    runner.advance_to_phase(Phase::PostCombatMain);
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Green]);
    runner.cast(late).resolve();
    assert!(!runner.state().objects[&late]
        .keywords
        .contains(&Keyword::FirstStrike));
    for id in [source, second] {
        assert!(runner.state().objects[&id]
            .keywords
            .contains(&Keyword::FirstStrike));
    }
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    // CR 514.2: only the granted keyword expires; printed Vigilance remains.
    for id in [source, second] {
        assert!(!runner.state().objects[&id]
            .keywords
            .contains(&Keyword::FirstStrike));
        assert!(runner.state().objects[&id]
            .keywords
            .contains(&Keyword::Vigilance));
    }
    assert_continuous_complete(runner.state());
}

#[test]
fn duration_repair_phase_keyword_sources_latch_after_paid_board_or_graveyard_departures() {
    for name in ["Odric, Lunarch Marshal", "Bleeding Effect"] {
        let face = duration_repair_face(name);
        let graveyard = name == "Bleeding Effect";
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = if graveyard {
            scenario.add_enchantment_from_oracle(P0, name, "").id()
        } else {
            scenario.add_creature(P0, name, 3, 3).id()
        };
        let other_source = if graveyard {
            scenario.add_enchantment_from_oracle(P1, name, "").id()
        } else {
            scenario.add_creature(P1, name, 3, 3).id()
        };
        let own = scenario.add_creature(P0, "Own phase recipient", 2, 9).id();
        let foreign = scenario
            .add_creature(P1, "Foreign phase recipient", 2, 9)
            .id();
        let donor = if graveyard {
            scenario
                .add_creature_to_graveyard(P0, "Graveyard first strike donor", 1, 8)
                .with_keyword(Keyword::FirstStrike)
                .id()
        } else {
            scenario
                .add_creature(P0, "Battlefield first strike donor", 1, 8)
                .with_keyword(Keyword::FirstStrike)
                .id()
        };
        if graveyard {
            scenario
                .add_creature_to_graveyard(P1, "Foreign graveyard trample donor", 1, 8)
                .with_keyword(Keyword::Trample);
        } else {
            scenario
                .add_creature(P1, "Foreign battlefield trample donor", 1, 8)
                .with_keyword(Keyword::Trample);
        }
        let remove = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                if graveyard { "Cremate" } else { "Unsummon" },
                true,
                if graveyard {
                    "Exile target card from a graveyard.\nDraw a card."
                } else {
                    "Return target creature to its owner's hand."
                },
            )
            .with_mana_cost(complete_publication_cost(if graveyard {
                "{B}"
            } else {
                "{U}"
            }))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(P0, "Disperse", true, U22_DISPERSE)
            .with_mana_cost(complete_publication_cost("{1}{U}"))
            .id();
        let late = scenario
            .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
            .with_mana_cost(complete_publication_cost("{1}{G}"))
            .id();
        for player in [P0, P1] {
            for _ in 0..8 {
                scenario.add_card_to_library_top(player, "Draw buffer");
            }
        }
        let mut runner = scenario.build();
        duration_repair_install_face(&mut runner, late, &duration_repair_face("Grizzly Bears"));
        for id in [source, other_source] {
            duration_repair_install_face(&mut runner, id, &face);
        }
        runner.advance_to_phase(Phase::BeginCombat);
        assert_eq!(runner.state().stack.len(), if graveyard { 1 } else { 2 });
        assert!(runner.state().stack.iter().any(|entry| matches!(entry.kind,engine::types::game_state::StackEntryKind::TriggeredAbility { source_id, .. } if source_id == source)));
        runner.advance_until_stack_empty();
        assert!(runner.state().objects[&own]
            .keywords
            .contains(&Keyword::FirstStrike));
        assert!(!runner.state().objects[&own]
            .keywords
            .contains(&Keyword::Flying));
        assert!(!runner.state().objects[&foreign]
            .keywords
            .contains(&Keyword::FirstStrike));
        assert_continuous_complete(runner.state());
        complete_publication_fund(
            &mut runner,
            P0,
            0,
            &[if graveyard {
                ManaType::Black
            } else {
                ManaType::Blue
            }],
        );
        runner.cast(remove).target_object(donor).resolve();
        assert_eq!(
            runner.state().objects[&donor].zone,
            if graveyard { Zone::Exile } else { Zone::Hand }
        );
        assert!(runner.state().objects[&own]
            .keywords
            .contains(&Keyword::FirstStrike));
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
        runner.cast(bounce).target_object(source).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
        assert!(runner.state().objects[&own]
            .keywords
            .contains(&Keyword::FirstStrike));
        assert_continuous_complete(runner.state());
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_phase(Phase::PostCombatMain);
        complete_publication_fund(&mut runner, P0, 1, &[ManaType::Green]);
        runner.cast(late).resolve();
        assert!(!runner.state().objects[&late]
            .keywords
            .contains(&Keyword::FirstStrike));
        assert!(runner.state().objects[&own]
            .keywords
            .contains(&Keyword::FirstStrike));
        assert_continuous_complete(runner.state());
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert!(!runner.state().objects[&own]
            .keywords
            .contains(&Keyword::FirstStrike));
        // A second genuine phase occurrence belongs to the differently controlled source.
        runner.advance_to_phase(Phase::BeginCombat);
        assert_eq!(runner.state().stack.len(), 1);
        assert!(
            matches!(runner.state().stack[0].kind,engine::types::game_state::StackEntryKind::TriggeredAbility { source_id, .. } if source_id == other_source)
        );
        runner.advance_until_stack_empty();
        assert!(runner.state().objects[&foreign]
            .keywords
            .contains(&Keyword::Trample));
        assert!(!runner.state().objects[&own]
            .keywords
            .contains(&Keyword::Trample));
        assert_continuous_complete(runner.state());
    }
}

#[test]
fn duration_repair_deckhand_paid_activations_bind_two_other_own_recipients() {
    let face = duration_repair_face("Departed Deckhand");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, &face.name, 2, 2).id();
    let first = scenario
        .add_creature(P0, "First selected recipient", 2, 10)
        .id();
    let second = scenario
        .add_creature(P0, "Second selected recipient", 3, 11)
        .id();
    let foreign = scenario
        .add_creature(P1, "Foreign illegal recipient", 3, 12)
        .id();
    let spirit = scenario
        .add_creature(P1, "Allowed Spirit", 1, 10)
        .with_subtypes(vec!["Spirit"])
        .id();
    let ordinary = scenario
        .add_creature(P1, "Forbidden non-Spirit", 1, 10)
        .id();
    for player in [P0, P1] {
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    duration_repair_install_face(&mut runner, source, &face);
    for selected in [second, first] {
        complete_publication_fund(&mut runner, P0, 3, &[ManaType::Blue]);
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: Default::default(),
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual Deckhand target selection")
        };
        assert_eq!(*player, P0);
        assert_eq!(target_slots.len(), 1);
        assert!(!target_slots[selection.current_slot].optional);
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(first)));
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(second)));
        assert!(!target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(source)));
        assert!(!target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(foreign)));
        assert!(runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(foreign))
            })
            .is_err());
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(selected)),
            })
            .unwrap();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().stack.len(), 1);
        runner.advance_until_stack_empty();
        assert!(engine::game::combat::can_block_pair(
            runner.state(),
            spirit,
            selected
        ));
        assert!(!engine::game::combat::can_block_pair(
            runner.state(),
            ordinary,
            selected
        ));
        assert_continuous_complete(runner.state());
    }
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    for selected in [first, second] {
        assert!(engine::game::combat::can_block_pair(
            runner.state(),
            ordinary,
            selected
        ));
    }
    assert_continuous_complete(runner.state());
}

// Grammar-axis twins use the same existing quoted activated-ability producer.
// They are labeled fixtures, not invented printed-card Oracle.
#[test]
fn duration_repair_unbounded_quoted_activation_works_later_while_finite_twin_ends() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let unbounded = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unbounded grant grammar fixture",
            false,
            "Target creature gains \"{T}: You gain 1 life.\"",
        )
        .id();
    let finite = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Finite grant grammar fixture",
            false,
            "Target creature gains \"{T}: You gain 1 life.\" until end of turn.",
        )
        .id();
    let first = scenario.add_creature(P0, "Unbounded recipient", 2, 8).id();
    let second = scenario.add_creature(P0, "Finite recipient", 3, 9).id();
    for player in [P0, P1] {
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    runner.cast(unbounded).target_object(first).resolve();
    runner.cast(finite).target_object(second).resolve();
    assert_eq!(runner.state().objects[&first].abilities.len(), 1);
    assert_eq!(runner.state().objects[&second].abilities.len(), 1);
    let life = runner.state().players[0].life;
    runner.activate(first, 0).resolve();
    runner.activate(second, 0).resolve();
    assert_eq!(runner.state().players[0].life, life + 2);
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().objects[&first].abilities.len(), 1);
    assert!(runner.state().objects[&second].abilities.is_empty());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    let life = runner.state().players[0].life;
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: second,
            ability_index: 0,
            payment_mode: Default::default()
        })
        .is_err());
    // CR 611.2a: the outer grant persists; the quoted body's action is unchanged.
    runner.activate(first, 0).resolve();
    assert_eq!(runner.state().players[0].life, life + 1);
    assert_continuous_complete(runner.state());
}

#[test]
fn duration_repair_supported_mutilate_quantity_remains_a_dynamic_population_preservation() {
    let face = duration_repair_face("Mutilate");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, &face.name, false, face.oracle_text.as_deref().unwrap())
        .with_mana_cost(complete_publication_cost("{2}{B}{B}"))
        .id();
    let own = scenario.add_creature(P0, "Own Mutilate member", 8, 20).id();
    let foreign = scenario
        .add_creature(P1, "Opposing Mutilate member", 9, 21)
        .id();
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.with_mana_pool(
        P0,
        complete_publication_pool(2, &[ManaType::Black, ManaType::Black]),
    );
    let mut runner = scenario.build();
    duration_repair_install_face(&mut runner, spell, &face);
    runner.cast(spell).resolve();
    assert_eq!(effective_pt(&mut runner, own), (6, 18));
    assert_eq!(effective_pt(&mut runner, foreign), (7, 19));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_continuous_complete(runner.state());
}

#[test]
fn duration_repair_licid_empty_legal_hosts_and_new_incarnation_use_public_actions() {
    let face = duration_repair_face("Calming Licid");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, &face.name, 2, 2).id();
    let host = scenario.add_creature(P0, "Legal other host", 4, 8).id();
    let greaves = scenario
        .add_artifact_from_oracle(P0, "Lightning Greaves", GREAVES)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(complete_publication_cost("{2}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(complete_publication_cost("{U}"))
        .id();
    scenario.with_mana_pool(
        P0,
        complete_publication_pool(0, &[ManaType::White, ManaType::Blue]),
    );
    let mut runner = scenario.build();
    duration_repair_install_face(&mut runner, source, &face);
    runner
        .act(GameAction::Equip {
            equipment_id: greaves,
            target_id: source,
        })
        .unwrap();
    let WaitingFor::EquipTarget {
        player,
        equipment_id,
        valid_targets,
    } = &runner.state().waiting_for
    else {
        panic!("actual Greaves equip target selection");
    };
    assert_eq!((*player, *equipment_id), (P0, greaves));
    let mut actual_targets = valid_targets.clone();
    actual_targets.sort();
    let mut expected_targets = vec![source, host];
    expected_targets.sort();
    assert_eq!(actual_targets, expected_targets);
    runner
        .act(GameAction::Equip {
            equipment_id: greaves,
            target_id: source,
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&source]
        .keywords
        .contains(&Keyword::Shroud));
    assert_continuous_complete(runner.state());
    let mut legal_twin = GameRunner::from_state(runner.state().clone());
    legal_twin.activate(source, 0).target_object(host).resolve();
    assert_eq!(
        legal_twin.state().objects[&source].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(host))
    );
    assert!(legal_twin.state().objects[&source]
        .card_types
        .subtypes
        .contains(&"Aura".into()));
    assert_continuous_complete(legal_twin.state());
    runner.cast(bounce).target_object(host).resolve();
    assert_eq!(runner.state().objects[&host].zone, Zone::Hand);
    let before = runner.state().players[0].mana_pool.total();
    // The source remains shrouded; no other creature remains to target. This is
    // the actual no-legal-target announcement path, not a synthetic empty prompt.
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: Default::default()
        })
        .is_err());
    assert_eq!(runner.state().players[0].mana_pool.total(), before);
    assert!(!runner.state().objects[&source].tapped);
    assert_continuous_complete(runner.state());

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, &face.name, 2, 2).id();
    let host = scenario
        .add_creature(P1, "Independent opposing host", 4, 8)
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Disperse", true, U22_DISPERSE)
        .with_mana_cost(complete_publication_cost("{1}{U}"))
        .id();
    for player in [P0, P1] {
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Draw buffer");
        }
    }
    let mut runner = scenario.build();
    duration_repair_install_face(&mut runner, source, &face);
    complete_publication_fund(&mut runner, P0, 0, &[ManaType::White]);
    runner.activate(source, 0).target_object(host).resolve();
    let birth = runner.state().objects[&source].incarnation;
    assert_eq!(
        runner.state().objects[&source].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(host))
    );
    assert_continuous_complete(runner.state());
    complete_publication_fund(&mut runner, P0, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(source).resolve();
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
    assert!(!runner.state().objects[&host].attachments.contains(&source));
    assert_continuous_complete(runner.state());
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(engine::game::combat::get_valid_attacker_ids(runner.state()).contains(&host));
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    complete_publication_fund(&mut runner, P0, 2, &[ManaType::White]);
    runner.cast(source).resolve();
    // CR 400.7: the fresh birth does not inherit its old Aura transformation.
    assert_ne!(runner.state().objects[&source].incarnation, birth);
    assert!(runner.state().objects[&source]
        .card_types
        .core_types
        .contains(&engine::types::card_type::CoreType::Creature));
    assert!(!runner.state().objects[&source]
        .card_types
        .subtypes
        .contains(&"Aura".into()));
    assert_eq!(runner.state().objects[&source].attached_to, None);
    assert_eq!(effective_pt(&mut runner, source), (2, 2));
    assert_continuous_complete(runner.state());
}
