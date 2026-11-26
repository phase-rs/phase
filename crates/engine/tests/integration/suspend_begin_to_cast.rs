//! CR 702.62a + CR 702.62c: a card can be suspended from hand exactly when its cast could begin.
//! `assert_suspend` reads the offer, the enforcement and the restriction alone; the last one leaves
//! out the gates on activated abilities. Every card has its full printed text.

use engine::ai_support::{activation_block_reasons, legal_actions};
use engine::game::casting::can_cast_object_now;
use engine::game::keywords::effective_sneak_cost;
use engine::game::layers::evaluate_layers;
use engine::game::restrictions::{
    ability_confined_to_sorcery_timing, check_activation_restrictions,
    spell_confined_to_sorcery_timing,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ChosenAttribute, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor, ManaCost};
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
    let (zone, entries) = if accepted {
        (Zone::Exile, 1)
    } else {
        (Zone::Hand, 0)
    };
    assert_eq!(copy.state().objects[&card].zone, zone);
    assert_eq!(copy.state().stack.len(), r.state().stack.len() + entries);
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

/// CR 601.3b: "you" on Leyline of Anticipation is its controller.
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

/// CR 601.3b: only casting a sorcery uses Quicken's permission up.
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

/// CR 805.4a: the step is "yours" for both members of the active team, and for no opponent.
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

/// CR 118.6: a missing mana cost, an unaffordable one and a missing target don't stop a cast from
/// beginning.
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

/// CR 602.5: a prohibited suspend isn't shown as merely unaffordable.
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

/// Pithing Needle prohibits no cast, so the restriction accepts. CR 116.2f makes suspending a
/// special action it doesn't stop; offer and enforcement still treat it as an activated ability.
#[test]
fn n1_pithing_needle_as_an_activated_ability() {
    let (mut r, (needle, gargadon)) = board(|s| {
        let needle = s
            .add_artifact_from_oracle(P1, "Pithing Needle", PITHING_NEEDLE)
            .id();
        (needle, gargadon(s))
    });
    name_card(&mut r, needle, "Rift Bolt");
    assert_suspend(&r, gargadon, true);
    name_card(&mut r, needle, "Greater Gargadon");
    assert_eq!(readings(&r, gargadon), [false, false, true]);
}
