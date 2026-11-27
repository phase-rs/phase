//! CR 702.62a + CR 702.62c: suspend's hand restriction checks whether the cast could begin.
//! `assert_suspend` checks the offer, enforcement and the restriction alone.

use engine::ai_support::{
    activation_block_reasons, candidate_actions, legal_actions, witness_payment_continuation,
    witness_payment_continuations, PaymentContinuationBatchStatus,
    PaymentContinuationIndeterminate,
};
use engine::game::casting::can_cast_object_now;
use engine::game::keywords::effective_sneak_cost;
use engine::game::layers::evaluate_layers;
use engine::game::restrictions::{
    ability_confined_to_sorcery_timing, check_activation_restrictions,
    spell_confined_to_sorcery_timing,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChosenAttribute, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor, ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GREATER_GARGADON: &str = "Suspend 10—{R}\nSacrifice an artifact, creature, or land: Remove a time counter from this card. Activate only if this card is suspended.";
const SUSPENDED_SENTENCE: &str = "Destroy target creature an opponent controls. That player loses 3 life. Exile Suspended Sentence with three time counters on it.\nSuspend 3—{1}{B} (Rather than cast this card from your hand, you may pay {1}{B} and exile it with three time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)";
const RIFT_BOLT: &str = "Rift Bolt deals 3 damage to any target.\nSuspend 1—{R} (Rather than cast this card from your hand, you may pay {R} and exile it with a time counter on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)";
const ANCESTRAL_VISION: &str = "Suspend 4—{U} (Rather than cast this card from your hand, pay {U} and exile it with four time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)\nTarget player draws three cards.";
const NANTUKO_SHAMAN: &str = "When this creature enters, if you control no tapped lands, draw a card.\nSuspend 1—{2}{G}{G} (Rather than cast this card from your hand, you may pay {2}{G}{G} and exile it with a time counter on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost. It has haste.)";
const ITH_HIGH_ARCANIST: &str = "Vigilance\n{T}: Untap target attacking creature. Prevent all combat damage that would be dealt to and dealt by that creature this turn.\nSuspend 4—{W}{U}";
const LOTUS_BLOOM: &str = "Suspend 3—{0} (Rather than cast this card from your hand, pay {0} and exile it with three time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)\n{T}, Sacrifice this artifact: Add three mana of any one color.";
const SEVENTEEN_YEAR_CICADAS: &str = "Create ten 1/1 white Insect creature tokens with flying. Exile 17-Year Cicadas with seventeen time counters on it.\nSuspend 17 — {0} (Rather than cast this card from your hand, you may pay {0} and exile it with seventeen time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)\nWhenever you cast a spell, if this card is suspended, remove a time counter from it.";
const AEON_CHRONICLER: &str = "Aeon Chronicler's power and toughness are each equal to the number of cards in your hand.\nSuspend X—{X}{3}{U}. X can't be 0.\nWhenever a time counter is removed from this card while it's exiled, draw a card.";
const BENALISH_COMMANDER: &str = "Benalish Commander's power and toughness are each equal to the number of Soldiers you control.\nSuspend X—{X}{W}{W}. X can't be 0.\nWhenever a time counter is removed from this card while it's exiled, create a 1/1 white Soldier creature token.";
const DETRITIVORE: &str = "Detritivore's power and toughness are each equal to the number of nonbasic land cards in your opponents' graveyards.\nSuspend X—{X}{3}{R}. X can't be 0. (Rather than cast this card from your hand, you may pay {X}{3}{R} and exile it with X time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost. It has haste.)\nWhenever a time counter is removed from this card while it's exiled, destroy target nonbasic land.";
const FUNGAL_BEHEMOTH: &str = "Fungal Behemoth's power and toughness are each equal to the number of +1/+1 counters on creatures you control.\nSuspend X—{X}{G}{G}. X can't be 0. (Rather than cast this card from your hand, you may pay {X}{G}{G} and exile it with X time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost. It has haste.)\nWhenever a time counter is removed from this card while it's exiled, you may put a +1/+1 counter on target creature.";
const ROILING_HORROR: &str = "Roiling Horror's power and toughness are each equal to your life total minus the life total of an opponent with the most life.\nSuspend X—{X}{B}{B}{B}. X can't be 0. (Rather than cast this card from your hand, you may pay {X}{B}{B}{B} and exile it with X time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost. It has haste.)\nWhenever a time counter is removed from this card while it's exiled, target player loses 1 life and you gain 1 life.";
const STEEL_GOLEM: &str = "You can't cast creature spells.";
const MEDDLING_MAGE: &str = "As this creature enters, choose a nonland card name.\nSpells with the chosen name can't be cast.";
const RULE_OF_LAW: &str = "Each player can't cast more than one spell each turn.";
const SILENCE: &str = "Your opponents can't cast spells this turn.";
const MEMORY_VESSEL: &str = "{T}, Exile this artifact: Each player exiles the top seven cards of their library. Until your next turn, players may play cards they exiled this way, and they can't play cards from their hand. Activate only as a sorcery.";
const TEFERI_TIME_RAVELER: &str = "Each opponent can cast spells only any time they could cast a sorcery.\n[+1]: Until your next turn, you may cast sorcery spells as though they had flash.\n[−3]: Return up to one target artifact, creature, or enchantment to its owner's hand. Draw a card.";
const TEFERI_MAGE_OF_ZHALFIR: &str = "Flash\nCreature cards you own that aren't on the battlefield have flash.\nEach opponent can cast spells only any time they could cast a sorcery.";
const CITY_OF_SOLITUDE: &str =
    "Players can cast spells and activate abilities only during their own turns.";
const SUDDEN_SHOCK: &str = "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nSudden Shock deals 2 damage to any target.";
const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const LEYLINE_OF_ANTICIPATION: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.";
const QUICKEN: &str = "The next sorcery spell you cast this turn can be cast as though it had flash. (It can be cast any time you could cast an instant.)\nDraw a card.";
const PRIMAL_PRAYERS: &str = "When this enchantment enters, you get {E}{E} (two energy counters).\nYou may cast creature spells with mana value 3 or less by paying {E} rather than paying their mana costs. If you cast a spell this way, you may cast it as though it had flash.";
const FIST_OF_SUNS: &str =
    "You may pay {W}{U}{B}{R}{G} rather than pay the mana cost for spells you cast.";
const BOBBY_ANOTHER_DWARF: &str = "Dwarf creature cards, legendary creature cards, and Equipment cards in your hand have sneak {2}. (You may cast a spell for its sneak cost if you also return an unblocked attacker you control to hand during the declare blockers step. Creatures with sneak enter tapped and attacking.)\nWhen an Equipment you control sneaks in, attach it to a creature you control.";
const THINK_TWICE: &str = "Draw a card.\nFlashback {2}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const GRAFDIGGERS_CAGE: &str = "Creature cards in graveyards and libraries can't enter the battlefield.\nPlayers can't cast spells from graveyards or libraries.";
const AVATARS_WRATH: &str = "Choose up to one target creature, then airbend all other creatures. (Exile them. While each one is exiled, its owner may cast it for {2} rather than its mana cost.)\nUntil your next turn, your opponents can't cast spells from anywhere other than their hands.\nExile Avatar's Wrath.";
const PITHING_NEEDLE: &str = "As this artifact enters, choose a card name.\nActivated abilities of sources with the chosen name can't be activated unless they're mana abilities.";

fn creature_card(
    s: &mut GameScenario,
    player: PlayerId,
    name: &str,
    (power, toughness): (i32, i32),
    cost: &str,
    text: &str,
) -> ObjectId {
    s.add_creature_to_hand(player, name, power, toughness)
        .from_oracle_text_with_keywords(&["Suspend"], text)
        .with_mana_cost(parse_mtgjson_mana_cost(cost))
        .id()
}

fn gargadon(s: &mut GameScenario) -> ObjectId {
    creature_card(
        s,
        P0,
        "Greater Gargadon",
        (9, 7),
        "{9}{R}",
        GREATER_GARGADON,
    )
}

fn shaman(s: &mut GameScenario) -> ObjectId {
    creature_card(s, P0, "Nantuko Shaman", (3, 2), "{2}{G}", NANTUKO_SHAMAN)
}

fn ith(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_creature_to_hand(player, "Ith, High Arcanist", 3, 5)
        .as_legendary()
        .from_oracle_text_with_keywords(&["Suspend", "Vigilance"], ITH_HIGH_ARCANIST)
        .with_mana_cost(parse_mtgjson_mana_cost("{5}{W}{U}"))
        .id()
}

fn spell(
    s: &mut GameScenario,
    player: PlayerId,
    name: &str,
    is_instant: bool,
    cost: &str,
    text: &str,
) -> ObjectId {
    s.add_spell_to_hand_from_oracle(player, name, is_instant, text)
        .with_mana_cost(parse_mtgjson_mana_cost(cost))
        .id()
}

fn sentence(s: &mut GameScenario) -> ObjectId {
    spell(
        s,
        P0,
        "Suspended Sentence",
        true,
        "{3}{B}",
        SUSPENDED_SENTENCE,
    )
}

fn rift_bolt(s: &mut GameScenario) -> ObjectId {
    spell(s, P0, "Rift Bolt", false, "{2}{R}", RIFT_BOLT)
}

fn leyline(s: &mut GameScenario, player: PlayerId) {
    s.add_enchantment_from_oracle(player, "Leyline of Anticipation", LEYLINE_OF_ANTICIPATION);
}

fn steel_golem(s: &mut GameScenario) {
    s.add_creature(P0, "Steel Golem", 3, 4)
        .as_artifact_creature()
        .from_oracle_text(STEEL_GOLEM);
}

fn zhalfir(s: &mut GameScenario, player: PlayerId) {
    s.add_creature(player, "Teferi, Mage of Zhalfir", 3, 4)
        .as_legendary()
        .from_oracle_text(TEFERI_MAGE_OF_ZHALFIR);
}

fn bobby(s: &mut GameScenario, player: PlayerId) {
    s.add_creature(player, "Bobby, Another Dwarf", 3, 3)
        .as_legendary()
        .from_oracle_text(BOBBY_ANOTHER_DWARF);
}

/// P0's precombat main; three of each basic land and eight library cards for each player.
fn board<T>(setup: impl FnOnce(&mut GameScenario) -> T) -> (GameRunner, T) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for color in ManaColor::ALL {
            for _ in 0..3 {
                s.add_basic_land(player, color);
            }
        }
        s.with_library_top(player, &["Wastes"; 8]);
    }
    let ids = setup(&mut s);
    let mut r = s.build();
    window(&mut r, P0, Phase::PreCombatMain);
    (r, ids)
}

/// `active`'s turn, `phase`, empty stack, P0 to act.
fn window(r: &mut GameRunner, active: PlayerId, phase: Phase) {
    let state = r.state_mut();
    assert!(state.stack.is_empty(), "window has an empty stack");
    state.active_player = active;
    state.phase = phase;
    give_priority(r, P0);
}

fn give_priority(r: &mut GameRunner, player: PlayerId) {
    let state = r.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

fn suspend_index(r: &GameRunner, card: ObjectId) -> Option<usize> {
    r.state().objects[&card]
        .abilities
        .iter()
        .position(|a| a.activation_zone == Some(Zone::Hand))
}

fn suspend_action(r: &GameRunner, card: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: card,
        ability_index: suspend_index(r, card).expect("suspend's hand ability"),
        payment_mode: CastPaymentMode::Auto,
    }
}

/// Suspends `card` on a copy of the game: `None` when refused.
fn suspended(r: &GameRunner, card: ObjectId) -> Option<GameRunner> {
    let mut copy = GameRunner::from_state(r.state().clone());
    let accepted = copy.act(suspend_action(r, card)).is_ok();
    let object = &copy.state().objects[&card];
    assert_eq!(object.zone, if accepted { Zone::Exile } else { Zone::Hand });
    assert_eq!(copy.state().stack, r.state().stack);
    if accepted {
        assert!(!object.face_down);
        assert!(object.counters[&CounterType::Time] > 0);
    }
    accepted.then_some(copy)
}

fn restriction_alone(r: &GameRunner, player: PlayerId, card: ObjectId) -> bool {
    let index = suspend_index(r, card).expect("suspend's hand ability");
    let restrictions = &r.state().objects[&card].abilities[index].activation_restrictions;
    check_activation_restrictions(r.state(), player, card, index, restrictions).is_ok()
}

/// Offered, accepted, and legal by the restriction alone.
fn readings(r: &GameRunner, card: ObjectId) -> [bool; 3] {
    [
        legal_actions(r.state()).contains(&suspend_action(r, card)),
        suspended(r, card).is_some(),
        restriction_alone(r, P0, card),
    ]
}

#[track_caller]
fn assert_suspend(r: &GameRunner, card: ObjectId, expected: bool) {
    assert_eq!(
        readings(r, card),
        [expected; 3],
        "offered, accepted, restriction"
    );
}

fn name_card(r: &mut GameRunner, permanent: ObjectId, name: &str) {
    let obj = r.state_mut().objects.get_mut(&permanent).unwrap();
    obj.chosen_attributes = vec![ChosenAttribute::CardName(name.into())];
    give_priority(r, P0);
}

fn tap_lands_except(r: &mut GameRunner, kept: Option<ObjectId>) {
    for (id, obj) in r.state_mut().objects.iter_mut() {
        if obj.card_types.core_types.contains(&CoreType::Land) && Some(*id) != kept {
            obj.tapped = true;
        }
    }
}

#[test]
fn l1_prohibited_card_type() {
    let (r, (gargadon, rift_bolt)) = board(|s| {
        steel_golem(s);
        (gargadon(s), rift_bolt(s))
    });
    assert_suspend(&r, gargadon, false);
    assert_suspend(&r, rift_bolt, true);
}

#[test]
fn l2_prohibited_name() {
    let (mut r, (mage, gargadon)) = board(|s| {
        let mage = s
            .add_creature_from_oracle(P1, "Meddling Mage", 2, 2, MEDDLING_MAGE)
            .id();
        (mage, gargadon(s))
    });
    name_card(&mut r, mage, "Rift Bolt");
    assert_suspend(&r, gargadon, true);
    name_card(&mut r, mage, "Greater Gargadon");
    assert_suspend(&r, gargadon, false);
}

#[test]
fn l3_cast_limit() {
    let (mut r, (memnite, gargadon)) = board(|s| {
        s.add_enchantment_from_oracle(P1, "Rule of Law", RULE_OF_LAW);
        let memnite = s
            .add_creature_to_hand(P0, "Memnite", 1, 1)
            .as_artifact_creature()
            .with_mana_cost(parse_mtgjson_mana_cost("{0}"))
            .id();
        (memnite, gargadon(s))
    });
    assert_suspend(&r, gargadon, true);
    r.cast(memnite)
        .resolve()
        .assert_zone(&[memnite], Zone::Battlefield);
    window(&mut r, P0, Phase::PreCombatMain);
    assert_suspend(&r, gargadon, false);
}

#[test]
fn l4_temporary_ban() {
    for (caster, expected) in [(P1, false), (P0, true)] {
        let (mut r, (silence, gargadon, sentence)) = board(|s| {
            let silence = spell(s, caster, "Silence", true, "{W}", SILENCE);
            (silence, gargadon(s), sentence(s))
        });
        assert_suspend(&r, gargadon, true);
        assert_suspend(&r, sentence, true);
        give_priority(&mut r, caster);
        r.cast(silence)
            .resolve()
            .assert_zone(&[silence], Zone::Graveyard);
        window(&mut r, P0, Phase::PreCombatMain);
        assert_suspend(&r, gargadon, expected);
        assert_suspend(&r, sentence, expected);
    }
}

#[test]
fn l5_no_play_from_hand() {
    let (mut r, (vessel, gargadon)) = board(|s| {
        let vessel = s
            .add_artifact_from_oracle(P0, "Memory Vessel", MEMORY_VESSEL)
            .id();
        (vessel, gargadon(s))
    });
    assert_suspend(&r, gargadon, true);
    r.activate(vessel, 0).resolve();
    window(&mut r, P0, Phase::PreCombatMain);
    assert_eq!(r.state().objects[&vessel].zone, Zone::Exile);
    assert_suspend(&r, gargadon, false);
}

#[test]
fn l6_sorcery_speed_lock() {
    let raveler = |s: &mut GameScenario, player| {
        s.add_planeswalker_from_oracle(
            player,
            "Teferi, Time Raveler",
            "Teferi",
            4,
            TEFERI_TIME_RAVELER,
        );
    };
    let locks: [fn(&mut GameScenario, PlayerId); 2] = [raveler, zhalfir];
    for lock in locks {
        let (mut r, sentence) = board(|s| {
            lock(s, P1);
            sentence(s)
        });
        assert_suspend(&r, sentence, true);
        window(&mut r, P0, Phase::Upkeep);
        assert_suspend(&r, sentence, false);
    }
}

#[test]
fn l7_own_turn_only() {
    let (mut r, sentence) = board(|s| {
        s.add_enchantment_from_oracle(P1, "City of Solitude", CITY_OF_SOLITUDE);
        sentence(s)
    });
    window(&mut r, P0, Phase::Upkeep);
    assert_suspend(&r, sentence, true);
    window(&mut r, P1, Phase::Upkeep);
    assert_suspend(&r, sentence, false);
}

/// CR 805.4a + CR 805.5a: Dosan permits the active team's nonrepresentative to suspend.
#[test]
fn l7_dosan_shared_team_turn() {
    let opponent = PlayerId(2);
    for (actor, dosan, expected) in [
        (P1, false, true),
        (opponent, false, true),
        (opponent, true, false),
        (P1, true, true),
    ] {
        let mut s = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
        s.at_phase(Phase::PreCombatMain);
        if dosan {
            s.add_creature(P0, "Dosan the Falling Leaf", 2, 2)
                .as_legendary()
                .from_oracle_text("Players can cast spells only during their own turns.");
        }
        leyline(&mut s, actor);
        s.add_basic_land(actor, ManaColor::Red);
        let card = creature_card(
            &mut s,
            actor,
            "Greater Gargadon",
            (9, 7),
            "{9}{R}",
            GREATER_GARGADON,
        );
        let mut r = s.build();
        evaluate_layers(r.state_mut());
        if actor == opponent {
            r.pass_holding_team().unwrap();
        }
        assert_eq!(r.state().active_player, P0);
        let holder = if actor == opponent { opponent } else { P0 };
        assert!(
            matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == holder)
        );
        let action = suspend_action(&r, card);
        let offered = engine::ai_support::legal_actions_for_viewer(r.state(), actor)
            .0
            .contains(&action);
        let result = engine::game::engine::apply(r.state_mut(), actor, action);
        assert_eq!(
            (offered, result.is_ok()),
            (expected, expected),
            "actor {actor:?}, Dosan {dosan}, result {result:?}"
        );
        assert_eq!(
            r.state().objects[&card].zone,
            if expected { Zone::Exile } else { Zone::Hand }
        );
    }
}

/// CR 702.61a: in P1's main phase, P0 responds to Sudden Shock (refused) or Giant Growth.
#[test]
fn l8_split_second() {
    for split_second in [true, false] {
        for teferi in [true, false] {
            let (mut r, (shock, growth, bear, gargadon, sentence)) = board(|s| {
                if teferi {
                    zhalfir(s, P0);
                }
                let shock = s
                    .add_spell_to_hand(P1, "Sudden Shock", true)
                    .from_oracle_text_with_keywords(&["Split second"], SUDDEN_SHOCK)
                    .with_mana_cost(parse_mtgjson_mana_cost("{1}{R}"))
                    .id();
                let growth = spell(s, P1, "Giant Growth", true, "{G}", GIANT_GROWTH);
                let bear = s.add_creature(P1, "Grizzly Bears", 2, 2).id();
                (shock, growth, bear, gargadon(s), sentence(s))
            });
            window(&mut r, P1, Phase::PreCombatMain);
            give_priority(&mut r, P1);
            if split_second {
                r.cast(shock).target_player(P0).commit();
            } else {
                r.cast(growth).target_object(bear).commit();
            }
            r.act(GameAction::PassPriority).unwrap();
            assert_eq!(r.state().stack.len(), 1);
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            ));
            assert_suspend(&r, sentence, !split_second);
            assert_suspend(&r, gargadon, teferi && !split_second);
        }
    }
}

/// CR 109.5: "you" on Leyline of Anticipation is its controller.
#[test]
fn t1_as_though_it_had_flash() {
    for (controller, expected) in [(Some(P0), true), (Some(P1), false), (None, false)] {
        let (mut r, gargadon) = board(|s| {
            if let Some(controller) = controller {
                leyline(s, controller);
            }
            gargadon(s)
        });
        for active in [P0, P1] {
            window(&mut r, active, Phase::Upkeep);
            assert_suspend(&r, gargadon, expected);
        }
    }
}

#[test]
fn t2_granted_flash() {
    let (mut r, (gargadon, rift_bolt)) = board(|s| {
        zhalfir(s, P0);
        (gargadon(s), rift_bolt(s))
    });
    window(&mut r, P1, Phase::Upkeep);
    assert_suspend(&r, gargadon, true);
    assert_suspend(&r, rift_bolt, false);
}

/// CR 116.2f: suspending is a special action and leaves Quicken's permission available.
#[test]
fn t3_next_spell() {
    let (mut r, (quicken, gargadon, rift_bolt)) = board(|s| {
        let quicken = spell(s, P0, "Quicken", true, "{U}", QUICKEN);
        (quicken, gargadon(s), rift_bolt(s))
    });
    window(&mut r, P1, Phase::Upkeep);
    assert_suspend(&r, rift_bolt, false);
    r.cast(quicken)
        .resolve()
        .assert_zone(&[quicken], Zone::Graveyard);
    window(&mut r, P1, Phase::Upkeep);
    assert_suspend(&r, gargadon, false);
    assert_suspend(&r, rift_bolt, true);
    let after = suspended(&r, rift_bolt).unwrap();
    assert_eq!(after.state().pending_next_spell_modifiers.len(), 1);
}

/// CR 601.3c: Primal Prayers' cost needn't be payable, nor be the grant this cast would apply.
#[test]
fn t4_unpaid_alternative_cost() {
    let primal_prayers = |controller, fist_of_suns: bool, energy| {
        let (mut r, ids) = board(|s| {
            if fist_of_suns {
                s.add_artifact_from_oracle(P0, "Fist of Suns", FIST_OF_SUNS);
            }
            s.add_enchantment_from_oracle(controller, "Primal Prayers", PRIMAL_PRAYERS);
            (shaman(s), gargadon(s))
        });
        r.state_mut().players[0].energy = energy;
        window(&mut r, P1, Phase::Upkeep);
        (r, ids)
    };
    for (controller, fist_of_suns, energy) in [
        (P0, false, 0),
        (P0, false, 1),
        (P0, true, 1),
        (P1, false, 1),
    ] {
        let (r, (shaman, gargadon)) = primal_prayers(controller, fist_of_suns, energy);
        assert_suspend(&r, shaman, controller == P0);
        assert_suspend(&r, gargadon, false);
    }
}

/// CR 702.190a: sneak opens a cast in your declare blockers step only.
#[test]
fn t5_sneak_window() {
    let (mut r, (ith, gargadon)) = board(|s| {
        bobby(s, P0);
        (ith(s, P0), gargadon(s))
    });
    assert!(effective_sneak_cost(r.state(), ith).is_some());
    for (active, phase, expected) in [
        (P0, Phase::DeclareBlockers, true),
        (P0, Phase::Upkeep, false),
        (P1, Phase::DeclareBlockers, false),
    ] {
        window(&mut r, active, phase);
        assert_suspend(&r, ith, expected);
        assert_suspend(&r, gargadon, false);
    }
}

/// CR 805.4 + CR 805.4a: the step is "yours" for both members of the active team, and for no opponent.
#[test]
fn t5_sneak_window_on_a_shared_team_turn() {
    for (player, expected) in [(P1, true), (PlayerId(2), false)] {
        let mut s = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
        s.at_phase(Phase::DeclareBlockers);
        bobby(&mut s, player);
        let ith = ith(&mut s, player);
        let mut r = s.build();
        r.state_mut().active_player = P0;
        give_priority(&mut r, player);
        assert!(effective_sneak_cost(r.state(), ith).is_some());
        assert_eq!(restriction_alone(&r, player, ith), expected);
    }
}

#[test]
fn t6_timing_query_reads_flash_routes_and_no_prohibitions() {
    let confined = |r: &GameRunner, card| {
        ability_confined_to_sorcery_timing(r.state(), P0, card, suspend_index(r, card).unwrap())
    };
    for (flash, golem) in [(false, false), (true, false), (true, true)] {
        let (r, gargadon) = board(|s| {
            if flash {
                leyline(s, P0);
            }
            if golem {
                steel_golem(s);
            }
            gargadon(s)
        });
        assert_eq!(confined(&r, gargadon), !flash);
    }
    let (mut r, ith) = board(|s| {
        bobby(s, P0);
        ith(s, P0)
    });
    window(&mut r, P0, Phase::Upkeep);
    assert!(confined(&r, ith));
    assert!(!spell_confined_to_sorcery_timing(r.state(), P0, ith));
}

/// CR 601.2 + CR 118.6: choosing targets and paying costs come after a cast begins.
#[test]
fn c1_completing_the_cast_is_irrelevant() {
    let (mut r, (mountain, vision, sentence, gargadon)) = board(|s| {
        let mountain = s.add_basic_land(P0, ManaColor::Red);
        let vision = s
            .add_spell_to_hand_from_oracle(P0, "Ancestral Vision", false, ANCESTRAL_VISION)
            .with_mana_cost(ManaCost::NoCost)
            .id();
        (mountain, vision, sentence(s), gargadon(s))
    });
    for card in [vision, sentence] {
        assert!(!can_cast_object_now(r.state(), P0, card));
        assert_suspend(&r, card, true);
    }
    tap_lands_except(&mut r, Some(mountain));
    assert!(!can_cast_object_now(r.state(), P0, gargadon));
    assert_suspend(&r, gargadon, true);
}

/// CR 702.62c: a prohibited suspend isn't shown as merely unaffordable.
#[test]
fn d1_block_reasons() {
    for golem in [false, true] {
        let (mut r, gargadon) = board(|s| {
            if golem {
                steel_golem(s);
            }
            gargadon(s)
        });
        tap_lands_except(&mut r, None);
        let blocked = activation_block_reasons(r.state());
        assert_eq!(blocked.contains_key(&gargadon), !golem);
    }
}

#[test]
fn x1_suspend_x_has_no_hand_ability() {
    for (name, cost, text) in [
        ("Aeon Chronicler", "{3}{U}{U}", AEON_CHRONICLER),
        ("Benalish Commander", "{3}{W}", BENALISH_COMMANDER),
        ("Detritivore", "{2}{R}{R}", DETRITIVORE),
        ("Fungal Behemoth", "{3}{G}", FUNGAL_BEHEMOTH),
        ("Roiling Horror", "{3}{B}{B}", ROILING_HORROR),
    ] {
        let (r, card) = board(|s| creature_card(s, P0, name, (0, 0), cost, text));
        let obj = &r.state().objects[&card];
        assert!(
            obj.keywords
                .iter()
                .any(|k| matches!(k, Keyword::Suspend { .. })),
            "{name}"
        );
        assert_eq!(suspend_index(&r, card), None, "{name}");
        assert!(
            !legal_actions(r.state()).iter().any(
                |a| matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == card)
            ),
            "{name}"
        );
        let has_trigger = |matches: fn(&Effect) -> bool| {
            obj.base_trigger_definitions.iter().any(|t| {
                t.trigger_zones == [Zone::Exile]
                    && t.execute.as_ref().is_some_and(|a| matches(&a.effect))
            })
        };
        assert!(
            has_trigger(|e| matches!(
                e,
                Effect::RemoveCounter {
                    target: TargetFilter::SelfRef,
                    ..
                }
            )),
            "{name}"
        );
        assert!(
            has_trigger(|e| matches!(
                e,
                Effect::CastFromZone {
                    target: TargetFilter::SelfRef,
                    ..
                }
            )),
            "{name}"
        );
    }
    let (r, (bloom, cicadas)) = board(|s| {
        let bloom = s
            .add_artifact_to_hand_from_oracle(P0, "Lotus Bloom", LOTUS_BLOOM)
            .with_mana_cost(ManaCost::NoCost)
            .id();
        (
            bloom,
            spell(
                s,
                P0,
                "17-Year Cicadas",
                false,
                "{7}{W}",
                SEVENTEEN_YEAR_CICADAS,
            ),
        )
    });
    assert_suspend(&r, bloom, true);
    assert_suspend(&r, cicadas, true);
}

fn think_twice(s: &mut GameScenario) -> ObjectId {
    s.add_spell_to_graveyard(P0, "Think Twice", true)
        .from_oracle_text_with_keywords(&["Flashback"], THINK_TWICE)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{U}"))
        .id()
}

/// Whether P0 can cast `card`, with the submitted cast agreeing.
fn cast_accepted(r: &GameRunner, card: ObjectId) -> bool {
    let mut copy = GameRunner::from_state(r.state().clone());
    let accepted = copy
        .act(GameAction::CastSpell {
            object_id: card,
            card_id: r.state().objects[&card].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .is_ok();
    let zone = if accepted {
        Zone::Stack
    } else {
        Zone::Graveyard
    };
    assert_eq!(copy.state().objects[&card].zone, zone);
    assert_eq!(can_cast_object_now(r.state(), P0, card), accepted);
    accepted
}

/// CR 601.3 + CR 101.2: the cast proposal still reads the zone check.
#[test]
fn z1_zone_check_still_stops_the_cast() {
    for cage in [false, true] {
        let (r, card) = board(|s| {
            if cage {
                s.add_artifact_from_oracle(P1, "Grafdigger's Cage", GRAFDIGGERS_CAGE);
            }
            think_twice(s)
        });
        assert_eq!(cast_accepted(&r, card), !cage);
        let offered = legal_actions(r.state())
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == card));
        assert_eq!(offered, !cage);
    }
}

/// CR 601.3: the cast proposal still reads the hand-only lock.
#[test]
fn z2_hand_only_lock_still_stops_the_cast() {
    let (mut r, (wrath, card)) = board(|s| {
        let wrath = spell(s, P1, "Avatar's Wrath", false, "{2}{W}{W}", AVATARS_WRATH);
        (wrath, think_twice(s))
    });
    window(&mut r, P1, Phase::PreCombatMain);
    assert!(cast_accepted(&r, card));
    give_priority(&mut r, P1);
    r.cast(wrath).resolve().assert_zone(&[wrath], Zone::Exile);
    window(&mut r, P1, Phase::PreCombatMain);
    assert!(!cast_accepted(&r, card));
}

/// CR 116.2f: naming Greater Gargadon with Pithing Needle cannot stop its suspend special action.
#[test]
fn n1_pithing_needle_allows_the_suspend_special_action() {
    let (mut r, (needle, gargadon)) = board(|s| {
        let needle = s
            .add_artifact_from_oracle(P1, "Pithing Needle", PITHING_NEEDLE)
            .id();
        let gargadon = s
            .add_creature_to_hand_from_oracle(P0, "Greater Gargadon", 9, 7, GREATER_GARGADON)
            .with_mana_cost(parse_mtgjson_mana_cost("{9}{R}"))
            .id();
        (needle, gargadon)
    });
    name_card(&mut r, needle, "Rift Bolt");
    assert_suspend(&r, gargadon, true);
    name_card(&mut r, needle, "Greater Gargadon");
    assert_suspend(&r, gargadon, true);

    let mut exiled = suspended(&r, gargadon).expect("hand action takes place under Needle");
    let index = exiled.state().objects[&gargadon]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                *ability.effect,
                engine::types::ability::Effect::RemoveCounter { .. }
            )
        })
        .expect("printed exile activation");
    let sacrifice = GameAction::ActivateAbility {
        source_id: gargadon,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    };
    assert!(!legal_actions(exiled.state()).contains(&sacrifice));
    assert!(exiled.act(sacrifice.clone()).is_err());
    name_card(&mut exiled, needle, "Rift Bolt");
    assert!(legal_actions(exiled.state()).contains(&sacrifice));
    assert!(matches!(
        exiled.act(sacrifice).unwrap().waiting_for,
        WaitingFor::PayCost { .. }
    ));
}

/// CR 702.62a: Suspended Sentence costs {1}{B} to suspend even with Zirda.
#[test]
fn n2_suspend_does_not_get_activated_ability_discounts() {
    for extra_land in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let swamp = s.add_basic_land(P0, ManaColor::Black);
        if extra_land {
            s.add_basic_land(P0, ManaColor::Green);
        }
        s.add_creature(P0, "Zirda, the Dawnwaker", 3, 3)
            .as_legendary()
            .from_oracle_text("Companion — Each permanent card in your starting deck has an activated ability.\nAbilities you activate that aren't mana abilities cost {2} less to activate. This effect can't reduce the mana in that cost to less than one mana.\n{1}, {T}: Target creature can't block this turn.");
        let millstone = s
            .add_artifact_from_oracle(P0, "Millstone", "{2}, {T}: Target player mills two cards.")
            .id();
        let milled = [
            s.add_land_to_library_top(P1, "Forest").id(),
            s.add_land_to_library_top(P1, "Island").id(),
        ];
        let card = sentence(&mut s);
        let mut r = s.build();
        window(&mut r, P0, Phase::PreCombatMain);
        assert_eq!(
            readings(&r, card),
            [extra_land, extra_land, true],
            "offer and enforcement require the full suspend cost"
        );
        if !extra_land {
            let mut paid_activation = GameRunner::from_state(r.state().clone());
            paid_activation
                .activate(millstone, 0)
                .target_player(P1)
                .resolve();
            assert!(paid_activation.state().objects[&swamp].tapped);
            assert!(paid_activation.state().objects[&millstone].tapped);
            assert_eq!(payment_pool(&paid_activation).total(), 0);
            assert!(milled
                .iter()
                .all(|id| { paid_activation.state().objects[id].zone == Zone::Graveyard }));
        }
    }
}

fn manual_suspend(r: &GameRunner, card: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: card,
        ability_index: suspend_index(r, card).expect("indexed Suspend action"),
        payment_mode: CastPaymentMode::Manual,
    }
}

fn choose_card(card: ObjectId) -> GameAction {
    GameAction::SelectCards { cards: vec![card] }
}

fn mana_ability(r: &GameRunner, object: ObjectId) -> GameAction {
    let (_, _, mut grouped) = engine::ai_support::legal_actions_for_viewer(r.state(), P0);
    let actions = grouped.remove(&object).unwrap_or_default();
    actions
        .into_iter()
        .find(|action| match action {
            GameAction::ActivateAbility { source_id, .. } => *source_id == object,
            GameAction::TapLandForMana { selection }
            | GameAction::ActivateManaSource { selection } => selection.source.object_id == object,
            _ => false,
        })
        .unwrap_or_else(|| {
            panic!(
                "expected a mana action for {object:?} during {:?}",
                r.state().waiting_for
            )
        })
}

fn tap_land(r: &mut GameRunner, land: ObjectId) {
    let action = mana_ability(r, land);
    r.act(action).expect("the land adds mana");
}

fn payment_pool(r: &GameRunner) -> &engine::types::mana::ManaPool {
    &r.state().players[P0.0 as usize].mana_pool
}

fn check_suspend_delivery(r: &GameRunner, card: ObjectId, count: u32) {
    let object = &r.state().objects[&card];
    assert_eq!(object.zone, Zone::Exile);
    assert!(!object.face_down);
    assert_eq!(object.counters[&CounterType::Time], count);
    assert!(r.state().stack.is_empty());
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

fn assert_counter_event(events: &[GameEvent], card: ObjectId, count: u32) {
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::CounterAdded {
            object_id, counter_type: CounterType::Time, count: added, actor: P0,
        } if *object_id == card && *added == count))
            .count(),
        1
    );
    assert!(!events.iter().any(
        |event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == card)
    ));
}

fn open_manual_suspend(r: &mut GameRunner, card: ObjectId, mana: &str) {
    let action = manual_suspend(r, card);
    assert!(
        legal_actions(r.state()).contains(&action),
        "Manual Suspend is offered"
    );
    let opened = r.act(action).expect("the offered action opens payment");
    assert!(matches!(
        opened.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        r.state().pending_cast.as_ref().expect("payment root").cost,
        parse_mtgjson_mana_cost(mana),
    );
    assert_eq!(r.state().objects[&card].zone, Zone::Hand);
    assert!(opened.events.iter().all(
        |event| !matches!(event, GameEvent::CounterAdded { object_id, .. } if *object_id == card)
    ));
}

fn familiar(s: &mut GameScenario) -> ObjectId {
    s.add_creature(P0, "Skirge Familiar", 3, 2)
        .from_oracle_text_with_keywords(&["Flying"], "Flying\nDiscard a card: Add {B}.")
        .id()
}

fn discard_choices(r: &GameRunner) -> &[ObjectId] {
    let WaitingFor::PayCost {
        player: P0,
        kind: PayCostKind::Discard,
        choices,
        count: 1,
        ..
    } = &r.state().waiting_for
    else {
        panic!(
            "expected the Familiar's discard cost, got {:?}",
            r.state().waiting_for
        );
    };
    choices
}

/// CR 118.3 + CR 702.62a: the hand action holds one exact occurrence for self-exile.
#[test]
fn suspend_payment_holds_the_announced_hand_occurrence() {
    for spare in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let swamp = s.add_basic_land(P0, ManaColor::Black);
        let skirge = familiar(&mut s);
        let card = sentence(&mut s);
        let extra = spare.then(|| sentence(&mut s));
        let mut r = s.build();
        assert!(restriction_alone(&r, P0, card));
        assert!(!legal_actions(r.state()).contains(&suspend_action(&r, card)));

        let Some(extra) = extra else {
            let manual = manual_suspend(&r, card);
            assert!(!legal_actions(r.state()).contains(&manual));
            assert!(r.act(manual).is_err());
            assert_eq!(r.state().objects[&card].zone, Zone::Hand);
            assert!(!r.state().objects[&swamp].tapped);
            assert_eq!(payment_pool(&r).total(), 0);

            r.act(mana_ability(&r, skirge))
                .expect("outside Suspend the Familiar can discard the card");
            assert_eq!(discard_choices(&r), &[card]);
            r.act(choose_card(card))
                .expect("complete the ordinary mana ability");
            assert_eq!(r.state().objects[&card].zone, Zone::Graveyard);
            assert_eq!(payment_pool(&r).count_color(ManaType::Black), 1);
            continue;
        };

        let mut canceled = GameRunner::from_state(r.state().clone());
        open_manual_suspend(&mut canceled, card, "{1}{B}");
        canceled
            .act(GameAction::CancelCast)
            .expect("cancel before using a mana source");
        assert!(canceled.state().pending_cast.is_none());
        canceled.act(mana_ability(&canceled, skirge)).unwrap();
        assert!(
            discard_choices(&canceled).contains(&card),
            "cancel releases the held occurrence"
        );

        open_manual_suspend(&mut r, card, "{1}{B}");
        r.act(mana_ability(&r, skirge))
            .expect("a distinct copy pays the Familiar");
        assert_eq!(discard_choices(&r), &[extra]);
        assert!(
            r.act(choose_card(card)).is_err(),
            "the same-name held card can't be submitted"
        );
        assert_eq!(discard_choices(&r), &[extra]);
        assert_eq!(payment_pool(&r).total(), 0);
        r.act(choose_card(extra))
            .expect("discard the distinct occurrence");
        tap_land(&mut r, swamp);
        assert_eq!(payment_pool(&r).count_color(ManaType::Black), 2);

        let mut stale = GameRunner::from_state(r.state().clone());
        stale
            .state_mut()
            .objects
            .get_mut(&card)
            .unwrap()
            .bump_incarnation();
        assert!(
            stale.act(GameAction::PassPriority).is_err(),
            "the prior hand occurrence cannot confirm"
        );
        assert_eq!(stale.state().objects[&card].zone, Zone::Hand);
        assert_eq!(payment_pool(&stale).count_color(ManaType::Black), 2);

        let finished = r
            .act(GameAction::PassPriority)
            .expect("spend the two eligible units");
        check_suspend_delivery(&r, card, 3);
        assert_eq!(r.state().objects[&extra].zone, Zone::Graveyard);
        assert_eq!(payment_pool(&r).total(), 0);
        assert_counter_event(&finished.events, card, 3);
    }
}

/// CR 118.3: two discard producers share the same expendable hand cards.
#[test]
fn suspend_payment_witness_counts_distinct_discards_and_certifies_completion() {
    for spare_count in [1, 2] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let sources = [familiar(&mut s), familiar(&mut s)];
        let card = sentence(&mut s);
        let spares: Vec<_> = (0..spare_count).map(|_| sentence(&mut s)).collect();
        let mut r = s.build();
        let manual = manual_suspend(&r, card);
        if spare_count == 1 {
            assert!(restriction_alone(&r, P0, card));
            assert!(!legal_actions(r.state()).contains(&manual));
            assert!(r.act(manual).is_err());
            continue;
        }

        open_manual_suspend(&mut r, card, "{1}{B}");
        let first = mana_ability(&r, sources[0]);
        assert!(witness_payment_continuation(r.state(), &first).is_some());
        for (source, spare) in sources.into_iter().zip(&spares) {
            r.act(mana_ability(&r, source)).unwrap();
            assert!(discard_choices(&r).contains(spare));
            assert!(!discard_choices(&r).contains(&card));
            r.act(choose_card(*spare)).unwrap();
        }
        let confirm = GameAction::PassPriority;
        let actions = payment_candidates(&r);
        let batch = witness_payment_continuations(r.state(), &actions);
        let certificate = batch.successors[actions
            .iter()
            .position(|action| *action == confirm)
            .unwrap()]
        .as_ref()
        .expect("actual stackless completion certifies the confirmation");
        let simulated = GameRunner::from_state(certificate.state.clone());
        check_suspend_delivery(&simulated, card, 3);
        assert_counter_event(&certificate.trace.events, card, 3);

        let finished = r.act(confirm).unwrap();
        check_suspend_delivery(&r, card, 3);
        assert_counter_event(&finished.events, card, 3);
        assert_eq!(payment_pool(&r).total(), 0);
        assert!(spares
            .iter()
            .all(|spare| r.state().objects[spare].zone == Zone::Graveyard));
    }
}

fn payment_candidates(r: &GameRunner) -> Vec<GameAction> {
    candidate_actions(r.state())
        .into_iter()
        .map(|candidate| candidate.action)
        .collect()
}

#[test]
fn suspend_payment_can_reveal_the_held_talisman_to_metalworker() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let metalworker = s.add_creature_from_oracle(P0, "Metalworker", 1, 2,
        "{T}: Reveal any number of artifact cards in your hand. Add {C}{C} for each card revealed this way.")
        .as_artifact().id();
    let card = s.add_artifact_to_hand_from_oracle(P0, "Sol Talisman",
        "Suspend 3—{1} (Rather than cast this card from your hand, pay {1} and exile it with three time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)\n{T}: Add {C}{C}.")
        .with_mana_cost(ManaCost::NoCost).id();
    let mut r = s.build();
    open_manual_suspend(&mut r, card, "{1}");
    let action = mana_ability(&r, metalworker);
    assert!(
        witness_payment_continuation(r.state(), &action).is_some(),
        "the Metalworker action has a certified path to Suspend completion"
    );
    r.act(action).expect("Metalworker opens its reveal prompt");
    let WaitingFor::RevealChoice {
        cards, player: P0, ..
    } = &r.state().waiting_for
    else {
        panic!(
            "expected the actual reveal choice, got {:?}",
            r.state().waiting_for
        );
    };
    assert_eq!(cards, &[card]);
    let mut empty = GameRunner::from_state(r.state().clone());
    empty
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("reveal zero cards");
    assert_eq!(payment_pool(&empty).total(), 0);
    assert!(empty.act(GameAction::PassPriority).is_err());

    let reveal = r.act(choose_card(card)).unwrap();
    assert!(reveal.events.iter().any(
        |event| matches!(event, GameEvent::CardsRevealed { card_ids, .. } if card_ids == &[card])
    ));
    assert_eq!(payment_pool(&r).count_color(ManaType::Colorless), 2);
    r.act(GameAction::PassPriority).unwrap();
    check_suspend_delivery(&r, card, 3);
    assert_eq!(payment_pool(&r).count_color(ManaType::Colorless), 1);
}

/// CR 605.3a + CR 702.62a: the Guide can fund the hand action from its own hand zone.
#[test]
fn suspend_payment_offers_a_hand_source_even_above_the_witness_limit() {
    for islands in [0, 63] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let lands: Vec<_> = (0..islands)
            .map(|_| s.add_basic_land(P0, ManaColor::Blue))
            .collect();
        let card = gargadon(&mut s);
        let guide = s
            .add_creature_to_hand_from_oracle(
                P0,
                "Simian Spirit Guide",
                2,
                2,
                "Exile this card from your hand: Add {R}.",
            )
            .id();
        let mut r = s.build();
        let card_incarnation = r.state().objects[&card].incarnation;
        let guide_incarnation = r.state().objects[&guide].incarnation;
        let undo = GameRunner::from_state(r.state().clone());
        assert_eq!(undo.state().objects[&card].zone, Zone::Hand);
        assert_eq!(undo.state().objects[&guide].zone, Zone::Hand);
        open_manual_suspend(&mut r, card, "{R}");
        let guide_action = mana_ability(&r, guide);
        let actions = payment_candidates(&r);
        let batch = witness_payment_continuations(r.state(), &actions);
        if islands > 0 {
            assert_eq!(
                actions
                    .iter()
                    .filter(|action| **action != GameAction::CancelCast)
                    .count(),
                65
            );
            assert_eq!(
                batch.status,
                PaymentContinuationBatchStatus::Indeterminate(
                    PaymentContinuationIndeterminate::OverRootCapacity
                )
            );
            assert!(batch.successors.iter().all(Option::is_none));
        } else {
            let guide_index = actions
                .iter()
                .position(|action| action == &guide_action)
                .unwrap();
            assert!(batch.successors[guide_index].is_some());
        }
        assert!(
            r.act(GameAction::PassPriority).is_err(),
            "unfunded confirmation never finishes"
        );
        r.act(GameAction::CancelCast)
            .expect("cancel before committing a mana source");
        for (id, incarnation) in [(card, card_incarnation), (guide, guide_incarnation)] {
            assert_eq!(r.state().objects[&id].zone, Zone::Hand);
            assert_eq!(r.state().objects[&id].incarnation, incarnation);
            assert!(r.state().objects[&id].counters.is_empty());
        }
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert!(r.state().pending_cast.is_none());
        assert_eq!(payment_pool(&r).total(), 0);

        open_manual_suspend(&mut r, card, "{R}");
        r.act(mana_ability(&r, guide)).unwrap();
        assert_eq!(payment_pool(&r).count_color(ManaType::Red), 1);
        assert_eq!(r.state().objects[&guide].zone, Zone::Exile);
        assert!(r.state().objects[&guide].counters.is_empty());
        let finished = r.act(GameAction::PassPriority).unwrap();
        check_suspend_delivery(&r, card, 10);
        assert_counter_event(&finished.events, card, 10);
        assert_eq!(payment_pool(&r).total(), 0);
        assert!(lands.iter().all(|land| !r.state().objects[land].tapped));
    }
}

/// CR 605.3a + CR 702.62a: a legal mana payment can lie beyond the AI's sacrifice shortlist.
#[test]
fn suspend_sacrifice_payment_can_use_a_choice_beyond_the_search_pool() {
    let db = crate::support::shared_card_db().expect("card fixture database");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    // Stage the battlefield after Murder destroys a Hangarback Walker with
    // twelve counters, and Soldevi Adnate has since entered and untapped.
    let thopters: Vec<_> = (0..12)
        .map(|_| {
            s.add_creature(P0, "Thopter", 1, 1)
                .as_artifact_creature()
                .with_subtypes(vec!["Thopter"])
                .from_oracle_text_with_keywords(&["Flying"], "Flying")
                .with_mana_cost(ManaCost::NoCost)
                .id()
        })
        .collect();
    let adnate = s.add_real_card(P0, "Soldevi Adnate", Zone::Battlefield, db);
    let card = sentence(&mut s);
    let mut r = s.build();
    for id in &thopters {
        r.state_mut().objects.get_mut(id).unwrap().is_token = true;
    }
    window(&mut r, P0, Phase::PreCombatMain);
    assert_eq!(payment_pool(&r).total(), 0, "reach: no floating mana");

    let manual = manual_suspend(&r, card);
    let mut unpayable = GameRunner::from_state(r.state().clone());
    unpayable
        .state_mut()
        .objects
        .get_mut(&adnate)
        .unwrap()
        .tapped = true;
    assert!(restriction_alone(&unpayable, P0, card));
    assert!(!legal_actions(unpayable.state()).contains(&manual));
    assert!(unpayable.act(manual.clone()).is_err());

    let offered = legal_actions(r.state()).contains(&manual);
    let opened = r
        .act(manual)
        .expect("Suspend can be paid by sacrificing the thirteenth choice");
    assert!(offered, "the payable Manual Suspend is offered");
    assert!(matches!(
        opened.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(r.state().objects[&card].zone, Zone::Hand);
    assert_eq!(
        r.state().pending_cast.as_ref().expect("payment root").cost,
        parse_mtgjson_mana_cost("{1}{B}"),
    );

    let produce = mana_ability(&r, adnate);
    r.act(produce).expect("activate Adnate during payment");
    let WaitingFor::PayCost {
        player: P0,
        kind: PayCostKind::Sacrifice,
        choices,
        count: 1,
        ..
    } = &r.state().waiting_for
    else {
        panic!(
            "expected Adnate's sacrifice choice, got {:?}",
            r.state().waiting_for
        );
    };
    assert_eq!(choices.len(), 13);
    assert_eq!(&choices[..12], thopters.as_slice());
    assert_eq!(choices[12], adnate, "the only payable choice is last");
    r.act(choose_card(adnate))
        .expect("Adnate can sacrifice itself");
    assert_eq!(r.state().objects[&adnate].zone, Zone::Graveyard);
    assert_eq!(payment_pool(&r).count_color(ManaType::Black), 2);
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    let finished = r.act(GameAction::PassPriority).expect("pay {1}{B}");
    assert_eq!(payment_pool(&r).total(), 0);
    check_suspend_delivery(&r, card, 3);
    assert_counter_event(&finished.events, card, 3);
}

/// CR 106.6: restricted blue from Guidelight and Omen doesn't pay Suspend; Hydraulic's does.
#[test]
fn only_restrictions_covering_special_actions_can_fund_suspend() {
    for (name, oracle, payable) in [
        (
            "Omen Hawker",
            "{T}: Add {C}{U}. Spend this mana only to activate abilities.",
            false,
        ),
        (
            "Guidelight Optimizer",
            "{T}: Add {U}. Spend this mana only to cast an artifact spell or activate an ability.",
            false,
        ),
        (
            "Hydraulic Helper",
            "Defender\n{T}: Add {U}. This mana can't be spent to cast a nonartifact spell.",
            true,
        ),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let producer = s.add_creature_from_oracle(P0, name, 2, 3, oracle).id();
        let key = s
            .add_artifact_from_oracle(P0, "Voltaic Key", "{1}, {T}: Untap target artifact.")
            .id();
        let card = spell(&mut s, P0, "Ancestral Vision", false, "", ANCESTRAL_VISION);
        let mut r = s.build();
        assert!(restriction_alone(&r, P0, card));
        let auto = suspend_action(&r, card);
        let manual = manual_suspend(&r, card);
        assert_eq!(legal_actions(r.state()).contains(&auto), payable, "{name}");
        if payable {
            let result = r
                .act(auto)
                .expect("Hydraulic's prohibition only applies to casting");
            check_suspend_delivery(&r, card, 4);
            assert_counter_event(&result.events, card, 4);
            assert!(r.state().objects[&producer].tapped);
            assert_eq!(payment_pool(&r).total(), 0);
        } else {
            let mut paid_activation = GameRunner::from_state(r.state().clone());
            assert_eq!(payment_pool(&paid_activation).total(), 0);
            paid_activation
                .act(mana_ability(&paid_activation, producer))
                .expect("the restricted mana source actually produces mana");
            assert!(paid_activation.state().objects[&producer].tapped);
            assert_eq!(
                payment_pool(&paid_activation).count_color(ManaType::Blue),
                1
            );
            let produced = payment_pool(&paid_activation).total();
            let outcome = paid_activation
                .activate(key, 0)
                .target_object(key)
                .resolve();
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { player: P0 }
            ));
            assert_eq!(payment_pool(&paid_activation).total(), produced - 1);
            assert!(!paid_activation.state().objects[&key].tapped);
            assert!(paid_activation.state().stack.is_empty());

            assert!(!legal_actions(r.state()).contains(&manual));
            assert!(r.act(auto).is_err());
            assert!(r.act(manual).is_err());
            assert!(!r.state().objects[&producer].tapped);
            assert_eq!(r.state().objects[&card].zone, Zone::Hand);
        }
    }
}

#[test]
fn suspend_manual_confirmation_leaves_legacy_activation_mana_unspent() {
    use engine::types::mana::{ManaRestriction, ManaUnit};
    let legacy: ManaRestriction = serde_json::from_value(serde_json::json!({
        "OnlyForTypeSpellsOrAbilities": { "spell_type": "Artifact", "ability": "Any" }
    }))
    .expect("decode the legacy restriction");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let card = sentence(&mut s);
    let skirge = familiar(&mut s);
    let spare = sentence(&mut s);
    let swamp = s.add_basic_land(P0, ManaColor::Black);
    let beacon = s
        .add_artifact_from_oracle(P0, "Voltaic Key", "{1}, {T}: Untap target artifact.")
        .id();
    let mut r = s.build();
    let denied = r
        .state_mut()
        .add_mana_to_pool(
            P0,
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![legacy]),
        )
        .expect("add and stamp the legacy unit");

    let mut can_activate = GameRunner::from_state(r.state().clone());
    can_activate
        .act(GameAction::ActivateAbility {
            source_id: beacon,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("a real activation accepts the legacy unit");
    assert!(can_activate.state().objects[&beacon].tapped);
    assert_eq!(payment_pool(&can_activate).total(), 0);
    assert!(!can_activate.state().stack.is_empty());

    open_manual_suspend(&mut r, card, "{1}{B}");
    assert!(r
        .act(GameAction::SpendPoolMana {
            pip_id: denied.pip_id
        })
        .is_err());
    tap_land(&mut r, swamp);
    assert!(
        r.act(GameAction::PassPriority).is_err(),
        "one eligible mana and one denied mana are insufficient"
    );
    r.act(mana_ability(&r, skirge)).unwrap();
    r.act(choose_card(spare)).unwrap();
    let result = r.act(GameAction::PassPriority).unwrap();
    check_suspend_delivery(&r, card, 3);
    assert_counter_event(&result.events, card, 3);
    assert_eq!(payment_pool(&r).total(), 1);
    assert_eq!(
        payment_pool(&r).units().next().unwrap().pip_id,
        denied.pip_id
    );
}

#[test]
fn suspend_does_not_use_creature_spell_spend_as() {
    use engine::types::mana::ManaUnit;
    for orrery in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_creature_from_oracle(P0, "Vizier of the Menagerie", 3, 4,
            "You may look at the top card of your library any time.\nYou may cast creature spells from the top of your library.\nYou can spend mana of any type to cast creature spells.");
        let source = orrery.then(|| s.add_artifact_from_oracle(P0, "Chromatic Orrery",
            "You may spend mana as though it were mana of any color.\n{T}: Add {C}{C}{C}{C}{C}.\n{5}, {T}: Draw a card for each color among permanents you control.").id());
        let card = gargadon(&mut s);
        let goblin = s
            .add_creature_to_hand_from_oracle(P0, "Raging Goblin", 1, 1, "Haste")
            .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
            .id();
        let mut r = s.build();
        if let Some(id) = source {
            r.state_mut().objects.get_mut(&id).unwrap().tapped = true;
        }
        r.state_mut()
            .add_mana_to_pool(
                P0,
                ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            )
            .unwrap();
        if orrery {
            let action = suspend_action(&r, card);
            assert!(legal_actions(r.state()).contains(&action));
            r.act(action).expect("board-wide spend-as pays Suspend");
            check_suspend_delivery(&r, card, 10);
        } else {
            assert_eq!(readings(&r, card), [false, false, true]);
            r.cast(goblin)
                .resolve()
                .assert_zone(&[goblin], Zone::Battlefield);
            assert_eq!(payment_pool(&r).total(), 0);
        }
    }
}

/// CR 111.10h + CR 106.6: the Powerstone producer covers Suspend's generic mana.
#[test]
fn powerstone_mana_can_pay_the_generic_suspend_cost() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let swamp = s.add_basic_land(P0, ManaColor::Black);
    let printed_ability = engine::game::effects::token::predefined_token_abilities("Powerstone")
        .pop()
        .expect("Powerstone has a real token-catalog mana ability");
    let powerstone = s
        .add_creature(P0, "Powerstone", 0, 0)
        .as_artifact()
        .with_ability_definition(printed_ability)
        .id();
    let card = sentence(&mut s);
    let mut r = s.build();
    let action = suspend_action(&r, card);
    assert!(legal_actions(r.state()).contains(&action));
    let finished = r.act(action).expect("Swamp and Powerstone pay {1}{B}");
    check_suspend_delivery(&r, card, 3);
    assert_counter_event(&finished.events, card, 3);
    assert!(r.state().objects[&swamp].tapped);
    assert!(r.state().objects[&powerstone].tapped);
    assert_eq!(payment_pool(&r).total(), 0);
}

/// CR 116.2f + CR 119.4: K'rrik pays a Suspend cost with life even under
/// Yasharn's prohibition, which still applies to a real spell.
#[test]
fn suspend_can_pay_life_under_yasharn_with_krrik() {
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::game_state::ShardChoice;

    let db = crate::support::shared_card_db().expect("card fixture database");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_real_card(P0, "K'rrik, Son of Yawgmoth", Zone::Battlefield, db);
    let yasharn = s.add_real_card(P1, "Yasharn, Implacable Earth", Zone::Battlefield, db);
    let lands = [
        s.add_real_card(P0, "Wastes", Zone::Battlefield, db),
        s.add_real_card(P0, "Wastes", Zone::Battlefield, db),
    ];
    let ordinary = s.add_real_card(P0, "Blood Artist", Zone::Hand, db);
    let card = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Living End",
            false,
            "Suspend 3—{2}{B}{B}\nEach player exiles all creature cards from their graveyard, then sacrifices all creatures they control, then puts all cards they exiled this way onto the battlefield.",
        )
        .with_mana_cost(ManaCost::NoCost)
        .id();
    let mut r = s.build();
    for land in lands {
        tap_land(&mut r, land);
    }
    let life = r.state().players[0].life;
    assert_eq!(payment_pool(&r).total(), 2);
    assert_eq!(payment_pool(&r).count_color(ManaType::Colorless), 2);
    let cast = GameAction::CastSpell {
        object_id: ordinary,
        card_id: r.state().objects[&ordinary].card_id,
        targets: Vec::new(),
        payment_mode: CastPaymentMode::Auto,
    };
    let mut prohibited = GameRunner::from_state(r.state().clone());
    assert!(!can_cast_object_now(prohibited.state(), P0, ordinary));
    assert!(prohibited.act(cast.clone()).is_err());
    assert_eq!(prohibited.state().objects[&ordinary].zone, Zone::Hand);
    let mut permitted = GameRunner::from_state(r.state().clone());
    engine::game::zones::move_to_zone(
        permitted.state_mut(),
        yasharn,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    window(&mut permitted, P0, Phase::PreCombatMain);
    assert!(can_cast_object_now(permitted.state(), P0, ordinary));
    let announced = permitted
        .act(cast)
        .expect("without Yasharn, K'rrik pays for Blood Artist");
    assert!(matches!(
        announced.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, ref shards, .. } if shards.len() == 1
    ));
    permitted
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayLife],
        })
        .expect("pay {B} with 2 life");
    assert_eq!(permitted.state().objects[&ordinary].zone, Zone::Stack);
    assert_eq!(permitted.state().players[0].life, life - 2);
    assert_eq!(payment_pool(&permitted).total(), 1);

    let action = suspend_action(&r, card);
    let offered = legal_actions(r.state()).contains(&action);
    let announced = r
        .act(action)
        .expect("Yasharn does not prohibit paying life to Suspend");
    assert!(offered);
    assert!(matches!(
        announced.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, ref shards, .. } if shards.len() == 2
    ));
    assert_eq!(r.state().players[0].life, life);
    assert_eq!(r.state().objects[&card].zone, Zone::Hand);
    let finished = r
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayLife; 2],
        })
        .expect("pay {2} and 4 life for Suspend");
    assert_eq!(r.state().players[0].life, life - 4);
    assert_eq!(payment_pool(&r).total(), 0);
    check_suspend_delivery(&r, card, 3);
    assert_counter_event(&finished.events, card, 3);
}
