//! CR 307.1 + CR 307.5: the engine's sorcery-timing queries answer through the
//! cast and activation legality authorities (parity in a non-sorcery window,
//! reach in the sorcery window).
//!
//! `spell_row` and `ability_row` ask the query in both windows, and compare it
//! with `can_cast_object_now` / `legal_actions` in the window that a
//! sorcery-confined subject is refused in. Every expected value comes from the
//! card and the rule, not from legality. Every real card below uses its
//! verbatim Oracle text.

use engine::ai_support::legal_actions;
use engine::game::casting::{activated_ability_definitions, can_cast_object_now};
use engine::game::combat::{AttackerInfo, CombatState};
use engine::game::layers::evaluate_layers;
use engine::game::perf_counters;
use engine::game::restrictions::{
    ability_confined_to_sorcery_timing, check_activation_restrictions,
    check_activation_restrictions_with_static_gates, spell_confined_to_sorcery_timing,
    ActivationRestrictionStaticGates,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, ActivationRestriction};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const OVERRUN: &str = "Creatures you control get +3/+3 and gain trample until end of turn. (Each of those creatures can deal excess combat damage to the player or planeswalker it's attacking.)";
const KARAIS_TECHNIQUE: &str = "Sneak {W}{B} (You may cast this spell for {W}{B} if you also return an unblocked attacker you control to hand during the declare blockers step.)\nChoose one or both —\n• Target creature gets +3/+3 until end of turn.\n• Target creature gets -3/-3 until end of turn.";
const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const LEYLINE_OF_ANTICIPATION: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.";
const ROUT: &str = "You may cast this spell as though it had flash if you pay {2} more to cast it. (You may cast it any time you could cast an instant.)\nDestroy all creatures. They can't be regenerated.";
const FLASH_PHOTOGRAPHY: &str = "You may cast this spell as though it had flash if it targets a permanent you control.\nCreate a token that's a copy of target permanent.\nFlashback {4}{U}{U}";
const PRIMAL_PRAYERS: &str = "When this enchantment enters, you get {E}{E} (two energy counters).\nYou may cast creature spells with mana value 3 or less by paying {E} rather than paying their mana costs. If you cast a spell this way, you may cast it as though it had flash.";
const AMBUSH_VIPER: &str = "Flash\nDeathtouch";
const PATRON_OF_THE_KITSUNE: &str = "Fox offering (You may cast this spell any time you could cast an instant by sacrificing a Fox and paying the difference in mana costs between this and the sacrificed Fox. Mana cost includes color.)\nWhenever a creature attacks, you may gain 1 life.";
const DEAFENING_CLARION: &str = "Choose one or both —\n• Deafening Clarion deals 3 damage to each creature.\n• Creatures you control gain lifelink until end of turn.";
const BRAVE_KIN_DUO: &str =
    "{1}, {T}: Target creature gets +1/+1 until end of turn. Activate only as a sorcery.";
const SHIVAN_DRAGON: &str = "Flying\n{R}: This creature gets +1/+0 until end of turn.";
const AZIMAET_DRAKE: &str =
    "Flying\n{U}: This creature gets +1/+0 until end of turn. Activate only once each turn.";
const ANCESTRAL_VISION: &str = "Suspend 4—{U} (Rather than cast this card from your hand, pay {U} and exile it with four time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)\nTarget player draws three cards.";
const SUSPENDED_SENTENCE: &str = "Destroy target creature an opponent controls. That player loses 3 life. Exile Suspended Sentence with three time counters on it.\nSuspend 3—{1}{B} (Rather than cast this card from your hand, you may pay {1}{B} and exile it with three time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)";
const BLUDGEON_BRAWL: &str = "Each noncreature, non-Equipment artifact is an Equipment with equip {X} and \"Equipped creature gets +X/+0,\" where X is that artifact's mana value.";
const MILLSTONE: &str = "{2}, {T}: Target player mills two cards.";
const LEONIN_SHIKARI: &str = "You may activate equip abilities any time you could cast an instant.";
const GARRUK_WILDSPEAKER: &str = "[+1]: Untap two target lands.\n[−1]: Create a 3/3 green Beast creature token.\n[−4]: Creatures you control get +3/+3 and gain trample until end of turn.";
const THE_WANDERING_EMPEROR: &str = "Flash\nAs long as The Wandering Emperor entered this turn, you may activate her loyalty abilities any time you could cast an instant.\n[+1]: Put a +1/+1 counter on up to one target creature. It gains first strike until end of turn.\n[−1]: Create a 2/2 white Samurai creature token with vigilance.\n[−2]: Exile target tapped creature. You gain 2 life.";

/// An id no object on any board carries.
const ABSENT: ObjectId = ObjectId(u64::MAX);

fn mana(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn overrun_cost() -> ManaCost {
    use ManaCostShard::Green;
    mana(2, vec![Green, Green, Green])
}

/// P0 at its precombat main with three of each basic land.
fn scenario() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for color in [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ] {
        for _ in 0..3 {
            s.add_basic_land(P0, color);
        }
    }
    s
}

fn add_overrun(s: &mut GameScenario) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, "Overrun", false, OVERRUN)
        .with_mana_cost(overrun_cost())
        .id()
}

fn add_leyline(s: &mut GameScenario, controller: PlayerId) {
    s.add_enchantment_from_oracle(
        controller,
        "Leyline of Anticipation",
        LEYLINE_OF_ANTICIPATION,
    );
}

fn add_garruk(s: &mut GameScenario) -> ObjectId {
    s.add_planeswalker_from_oracle(P0, "Garruk Wildspeaker", "Garruk", 4, GARRUK_WILDSPEAKER)
        .id()
}

/// The Wandering Emperor under P0. Follow with `set_emperor_entered`.
fn add_emperor(s: &mut GameScenario) -> ObjectId {
    s.add_planeswalker_from_oracle(P0, "The Wandering Emperor", "", 3, THE_WANDERING_EMPEROR)
        .id()
}

/// As `granted_loyalty_ability_timing.rs` builds her: she has no planeswalker
/// subtype, and "entered this turn" reads `entered_battlefield_turn`.
fn set_emperor_entered(runner: &mut GameRunner, emperor: ObjectId, entered_this_turn: bool) {
    let state = runner.state_mut();
    let turn = state.turn_number;
    let obj = state.objects.get_mut(&emperor).unwrap();
    obj.card_types.subtypes.clear();
    obj.base_card_types = obj.card_types.clone();
    obj.entered_battlefield_turn = Some(if entered_this_turn { turn } else { turn - 1 });
}

/// Put the game in a priority window: `active` player's turn, `phase`, P0 to
/// act, empty stack; then run the layer system, which refreshes the static-mode
/// presence index that the gates and the `CastWithKeyword` scan read.
fn set_window(runner: &mut GameRunner, active: PlayerId, phase: Phase) {
    let state = runner.state_mut();
    state.active_player = active;
    state.phase = phase;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    assert!(state.stack.is_empty(), "fixture starts with an empty stack");
    evaluate_layers(state);
}

fn castable(runner: &GameRunner, id: ObjectId) -> bool {
    can_cast_object_now(runner.state(), P0, id)
}

fn spell_query(runner: &GameRunner, id: ObjectId) -> bool {
    spell_confined_to_sorcery_timing(runner.state(), P0, id)
}

fn ability_query(runner: &GameRunner, id: ObjectId, index: usize) -> bool {
    ability_confined_to_sorcery_timing(runner.state(), P0, id, index)
}

fn offered(runner: &GameRunner, id: ObjectId, index: usize) -> bool {
    legal_actions(runner.state()).contains(&GameAction::ActivateAbility {
        source_id: id,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    })
}

/// The query in both windows. Own precombat main: the object is castable, so a
/// later refusal is about timing. Own upkeep: the cast authority's refusal
/// matches the query.
fn spell_row(runner: &mut GameRunner, id: ObjectId, expected_confined: bool) {
    set_window(runner, P0, Phase::PreCombatMain);
    assert!(castable(runner, id), "reach: castable in the own main");
    assert_eq!(
        spell_query(runner, id),
        expected_confined,
        "query, own main"
    );

    set_window(runner, P0, Phase::Upkeep);
    assert_eq!(
        spell_query(runner, id),
        expected_confined,
        "query, own upkeep"
    );
    assert_eq!(
        !castable(runner, id),
        expected_confined,
        "parity with can_cast_object_now, own upkeep"
    );
}

/// `spell_row`'s activation twin, against `legal_actions`.
fn ability_row(runner: &mut GameRunner, id: ObjectId, index: usize, expected_confined: bool) {
    set_window(runner, P0, Phase::PreCombatMain);
    assert!(offered(runner, id, index), "reach: offered in the own main");
    assert_eq!(
        ability_query(runner, id, index),
        expected_confined,
        "query, own main"
    );

    set_window(runner, P0, Phase::Upkeep);
    assert_eq!(
        ability_query(runner, id, index),
        expected_confined,
        "query, own upkeep"
    );
    assert_eq!(
        !offered(runner, id, index),
        expected_confined,
        "parity with legal_actions, own upkeep"
    );
}

/// Index of the loyalty ability costing `amount`.
fn loyalty_index(runner: &GameRunner, id: ObjectId, amount: i32) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|a| matches!(a.cost, Some(AbilityCost::Loyalty { amount: n }) if n == amount))
        .expect("the loyalty ability is printed")
}

/// Index of the only activated ability of a card with one.
fn only_activated_index(runner: &GameRunner, id: ObjectId) -> usize {
    let definitions = activated_ability_definitions(runner.state(), id);
    assert_eq!(definitions.len(), 1, "fixture has one activated ability");
    definitions[0].0
}

/// Index of `card`'s suspend ability, found where `legal_actions` offers it.
/// The card has no other ability that can be activated from hand.
fn offered_hand_ability_index(runner: &GameRunner, card: ObjectId) -> usize {
    legal_actions(runner.state())
        .into_iter()
        .find_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if source_id == card => Some(ability_index),
            _ => None,
        })
        .expect("suspend is offered from hand in the own main")
}

/// S0, S1, S2, S4. A sorcery has no route. An instant's own timing and printed
/// Flash are not sorcery timing. A missing object makes no claim.
#[test]
fn spell_query_with_no_route_or_as_an_instant() {
    let mut s = scenario();
    s.add_creature(P0, "Grizzly Bears", 2, 2);
    let overrun = add_overrun(&mut s);
    let giant_growth = s
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
        .with_mana_cost(mana(0, vec![ManaCostShard::Green]))
        .id();
    let ambush_viper = s
        .add_creature_to_hand_from_oracle(P0, "Ambush Viper", 2, 1, AMBUSH_VIPER)
        .with_mana_cost(mana(1, vec![ManaCostShard::Green]))
        .id();
    let mut r = s.build();

    spell_row(&mut r, overrun, true);
    spell_row(&mut r, giant_growth, false);
    spell_row(&mut r, ambush_viper, false);

    // S0's reach is `overrun` reading confined on this board.
    assert!(!spell_query(&r, ABSENT), "a missing object makes no claim");
}

/// S3a, S3b. CR 601.3b: "you" on Leyline of Anticipation is its controller.
#[test]
fn granted_flash_is_scoped_to_the_permissions_controller() {
    for (leyline_controller, expected_confined) in [(P0, false), (P1, true)] {
        let mut s = scenario();
        add_leyline(&mut s, leyline_controller);
        let overrun = add_overrun(&mut s);
        let mut r = s.build();
        spell_row(&mut r, overrun, expected_confined);
    }
}

/// S5. CR 601.3c: the option's surcharge is a cost, not a timing gate.
#[test]
fn flash_option_opens_the_route_whatever_its_surcharge() {
    use ManaCostShard::White;
    let mut s = scenario();
    let rout = s
        .add_spell_to_hand_from_oracle(P0, "Rout", false, ROUT)
        .with_mana_cost(mana(3, vec![White, White]))
        .id();
    let mut r = s.build();
    spell_row(&mut r, rout, false);
}

/// S6a, S6b. CR 601.3d: the option's condition has to be able to be met. P1's
/// creature keeps the card castable when P0 controls no permanent.
#[test]
fn target_dependent_flash_option_needs_a_qualifying_target() {
    use ManaCostShard::Blue;
    let photography_cost = || mana(2, vec![Blue, Blue]);

    let mut feasible = scenario();
    let photography = feasible
        .add_spell_to_hand_from_oracle(P0, "Flash Photography", false, FLASH_PHOTOGRAPHY)
        .with_mana_cost(photography_cost())
        .id();
    let mut r = feasible.build();
    spell_row(&mut r, photography, false);

    let mut infeasible = GameScenario::new();
    infeasible.at_phase(Phase::PreCombatMain);
    infeasible.add_creature(P1, "Grizzly Bears", 2, 2);
    infeasible.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(1), false, Vec::new()))
            .collect(),
    );
    let photography = infeasible
        .add_spell_to_hand_from_oracle(P0, "Flash Photography", false, FLASH_PHOTOGRAPHY)
        .with_mana_cost(photography_cost())
        .id();
    let mut r = infeasible.build();
    spell_row(&mut r, photography, true);
}

/// S7a, S7b. CR 118.9 + CR 118.3: "If you cast a spell this way" gives the
/// flash timing only to a cast that can pay {E}.
#[test]
fn alternative_cost_timing_needs_a_payable_cost() {
    for (energy, expected_confined) in [(1, false), (0, true)] {
        let mut s = scenario();
        s.add_enchantment_from_oracle(P0, "Primal Prayers", PRIMAL_PRAYERS);
        let bears = s
            .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
            .with_mana_cost(mana(1, vec![ManaCostShard::Green]))
            .id();
        let mut r = s.build();
        r.state_mut().players[0].energy = energy;
        spell_row(&mut r, bears, expected_confined);
    }
}

/// S8a, S8b. CR 702.48a + CR 701.21a: the offering is open only for a permanent
/// the caster can sacrifice.
#[test]
fn offering_needs_a_permanent_the_caster_can_sacrifice() {
    use ManaCostShard::White;
    for (fox_controller, expected_confined) in [(P0, false), (P1, true)] {
        let mut s = scenario();
        s.add_creature(fox_controller, "Fox", 1, 1)
            .with_subtypes(vec!["Fox"]);
        // The inline Oracle parse does not produce the offering keyword, so it
        // is set as the `casting_costs.rs` offering tests set it.
        let patron = s
            .add_creature_to_hand_from_oracle(
                P0,
                "Patron of the Kitsune",
                5,
                6,
                PATRON_OF_THE_KITSUNE,
            )
            .with_mana_cost(mana(4, vec![White, White]))
            .with_keyword(Keyword::Offering("Fox".to_string()))
            .id();
        let mut r = s.build();
        spell_row(&mut r, patron, expected_confined);
    }
}

/// S10. CR 702.190a: Karai's Technique is cast in the declare blockers step, so
/// it is not confined, although it is refused in the upkeep. Overrun, on the
/// same board, is confined and is not offered.
#[test]
fn sneak_opens_a_declare_blockers_cast() {
    use ManaCostShard::{Black, White};
    let mut s = scenario();
    let attacker = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let karai = s
        .add_spell_to_hand_from_oracle(P0, "Karai's Technique", false, KARAIS_TECHNIQUE)
        .with_mana_cost(mana(1, vec![White, Black]))
        .id();
    let overrun = add_overrun(&mut s);
    let mut r = s.build();
    assert!(
        r.state().objects[&karai]
            .keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Sneak(_))),
        "premise: the Oracle line parses to Sneak"
    );

    set_window(&mut r, P0, Phase::PreCombatMain);
    assert!(castable(&r, karai), "reach: castable in the own main");
    assert!(!spell_query(&r, karai));
    assert!(spell_query(&r, overrun));

    // The query reads Sneak by presence, so it does not match the refusal here.
    set_window(&mut r, P0, Phase::Upkeep);
    assert!(!spell_query(&r, karai));
    assert!(!castable(&r, karai));

    r.state_mut().combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
        ..Default::default()
    });
    set_window(&mut r, P0, Phase::DeclareBlockers);
    let actions = legal_actions(r.state());
    let sneak_offered = |object: ObjectId| {
        actions.iter().any(|action| {
            matches!(
                action,
                GameAction::CastSpellAsSneak { hand_object, .. } if *hand_object == object
            )
        })
    };
    assert!(sneak_offered(karai), "the reference reading: Sneak's offer");
    assert!(!sneak_offered(overrun));
}

/// N2, N2-L. The contract phase 3 relies on: at the spell's own mode prompt the
/// query answers as it did when the cast was proposed. Pinned, not derived from
/// a rule; mid-cast, the spell is already being cast.
#[test]
fn mode_prompt_reads_as_the_proposed_cast() {
    use ManaCostShard::{Red, White};
    for (leyline, expected_confined) in [(false, true), (true, false)] {
        let mut s = scenario();
        if leyline {
            add_leyline(&mut s, P0);
        }
        let clarion = s
            .add_spell_to_hand_from_oracle(P0, "Deafening Clarion", false, DEAFENING_CLARION)
            .with_mana_cost(mana(1, vec![Red, White]))
            .id();
        let mut r = s.build();
        set_window(&mut r, P0, Phase::PreCombatMain);
        let pre = spell_query(&r, clarion);
        assert_eq!(pre, expected_confined, "query at the proposal");

        let card_id = r.state().objects[&clarion].card_id;
        r.act(GameAction::CastSpell {
            object_id: clarion,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Deafening Clarion's cast begins");

        let WaitingFor::ModeChoice {
            player,
            pending_cast,
            ..
        } = r.state().waiting_for.clone()
        else {
            panic!("expected the mode prompt: {:?}", r.state().waiting_for);
        };
        assert_eq!(player, P0);
        assert_eq!(pending_cast.object_id, clarion);
        // CR 601.2a: the spell is on the stack; the object keeps its origin zone
        // until `finalize_cast`.
        assert_eq!(r.state().objects[&clarion].zone, Zone::Hand);
        assert!(
            r.state().stack.iter().any(|e| e.source_id == clarion),
            "the announced spell has a stack entry"
        );
        assert_eq!(spell_query(&r, pending_cast.object_id), pre);
    }
}

/// A1, A2, A3, A4, A6a, A6b. Timing restrictions confine; other restrictions
/// and no restriction do not. CR 702.62a: card cast timing follows the type.
#[test]
fn activation_query_classifies_each_timing_restriction() {
    use ManaCostShard::{Black, Blue, Red, White};
    let mut s = scenario();
    let garruk = add_garruk(&mut s);
    let brave_kin = s
        .add_creature_from_oracle(P0, "Brave-Kin Duo", 1, 1, BRAVE_KIN_DUO)
        .with_mana_cost(mana(0, vec![White]))
        .id();
    let shivan = s
        .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, SHIVAN_DRAGON)
        .with_mana_cost(mana(4, vec![Red, Red]))
        .id();
    let azimaet = s
        .add_creature_from_oracle(P0, "Azimaet Drake", 1, 3, AZIMAET_DRAKE)
        .with_mana_cost(mana(2, vec![Blue]))
        .id();
    let vision = s
        .add_spell_to_hand_from_oracle(P0, "Ancestral Vision", false, ANCESTRAL_VISION)
        .id();
    let sentence = s
        .add_spell_to_hand_from_oracle(P0, "Suspended Sentence", true, SUSPENDED_SENTENCE)
        .with_mana_cost(mana(3, vec![Black]))
        .id();
    let mut r = s.build();

    let minus_four = loyalty_index(&r, garruk, -4);
    ability_row(&mut r, garruk, minus_four, true);

    let index = only_activated_index(&r, brave_kin);
    ability_row(&mut r, brave_kin, index, true);

    let index = only_activated_index(&r, shivan);
    ability_row(&mut r, shivan, index, false);

    let index = only_activated_index(&r, azimaet);
    ability_row(&mut r, azimaet, index, false);

    set_window(&mut r, P0, Phase::PreCombatMain);
    let suspend = offered_hand_ability_index(&r, vision);
    ability_row(&mut r, vision, suspend, true);

    set_window(&mut r, P0, Phase::PreCombatMain);
    let suspend = offered_hand_ability_index(&r, sentence);
    ability_row(&mut r, sentence, suspend, false);
}

/// A5a, A5b, A5c. CR 304.5: the Emperor's permission holds the turn she entered
/// and covers her loyalty abilities, not another planeswalker's.
#[test]
fn activate_as_instant_permission_lifts_only_its_own_abilities() {
    for (entered_this_turn, expected_confined) in [(true, false), (false, true)] {
        let mut s = scenario();
        let emperor = add_emperor(&mut s);
        let mut r = s.build();
        set_emperor_entered(&mut r, emperor, entered_this_turn);
        let plus_one = loyalty_index(&r, emperor, 1);
        ability_row(&mut r, emperor, plus_one, expected_confined);
    }

    let mut s = scenario();
    let emperor = add_emperor(&mut s);
    let garruk = add_garruk(&mut s);
    let mut r = s.build();
    set_emperor_entered(&mut r, emperor, true);
    let plus_one = loyalty_index(&r, emperor, 1);
    let minus_four = loyalty_index(&r, garruk, -4);
    ability_row(&mut r, garruk, minus_four, true);
    assert!(
        offered(&r, emperor, plus_one),
        "reach: the permission is in force in the same upkeep"
    );
}

/// Bludgeon Brawl's board: Millstone carries a layer-granted equip ability,
/// which `runtime_granted_equip_abilities` appends past the printed list.
/// Returns the artifact and the granted index.
fn equip_board(shikari: bool) -> (GameRunner, ObjectId, usize) {
    let mut s = scenario();
    s.add_enchantment_from_oracle(P0, "Bludgeon Brawl", BLUDGEON_BRAWL);
    s.add_creature(P0, "Grizzly Bears", 2, 2);
    if shikari {
        s.add_creature_from_oracle(P0, "Leonin Shikari", 2, 2, LEONIN_SHIKARI);
    }
    let millstone = s
        .add_artifact_from_oracle(P0, "Millstone", MILLSTONE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut r = s.build();
    set_window(&mut r, P0, Phase::PreCombatMain);

    let printed = r.state().objects[&millstone].abilities.len();
    let granted = activated_ability_definitions(r.state(), millstone)
        .into_iter()
        .map(|(index, _)| index)
        .find(|index| *index >= printed)
        .expect("reach: the layers grant Millstone an equip ability past its printed list");
    (r, millstone, granted)
}

/// A7a, A7b. CR 702.6a + CR 304.5: a runtime-granted equip ability is read
/// through the activation lookup, and a keyword-tagged permission lifts it.
#[test]
fn runtime_granted_equip_is_read_through_the_activation_lookup() {
    let (mut r, millstone, equip) = equip_board(false);
    ability_row(&mut r, millstone, equip, true);

    let (mut r, millstone, equip) = equip_board(true);
    ability_row(&mut r, millstone, equip, false);
}

/// S0's twin, A8 and MS. A missing subject makes no claim, while legality's
/// results for a missing source are preserved.
#[test]
fn missing_subjects_make_no_claim_and_legality_still_refuses() {
    let mut s = scenario();
    let shivan = s
        .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, SHIVAN_DRAGON)
        .id();
    let brave_kin = s
        .add_creature_from_oracle(P0, "Brave-Kin Duo", 1, 1, BRAVE_KIN_DUO)
        .id();
    let mut r = s.build();
    set_window(&mut r, P0, Phase::PreCombatMain);

    let index = only_activated_index(&r, brave_kin);
    assert!(ability_query(&r, brave_kin, index), "reach: confined");
    assert!(!ability_query(&r, ABSENT, 0), "absent source");
    assert!(!ability_query(&r, shivan, 99), "out-of-range index");

    let refused = |r: &GameRunner, restriction: ActivationRestriction| {
        check_activation_restrictions(r.state(), P0, ABSENT, 0, &[restriction]).is_err()
    };
    assert!(refused(&r, ActivationRestriction::MatchesCardCastTiming));
    assert!(!refused(&r, ActivationRestriction::AsSorcery));

    set_window(&mut r, P0, Phase::Upkeep);
    assert!(refused(&r, ActivationRestriction::MatchesCardCastTiming));
    assert!(refused(&r, ActivationRestriction::AsSorcery));
}

/// PF-1. Decision 2.3: legality's delegated arms reuse the caller's gates, and
/// the window comes first, so the permission scan runs only outside it.
#[test]
fn delegated_legality_arms_compute_no_gates_and_test_the_window_first() {
    let mut s = scenario();
    let garruk = add_garruk(&mut s);
    let emperor = add_emperor(&mut s);
    let mut r = s.build();
    set_emperor_entered(&mut r, emperor, true);
    let minus_four = loyalty_index(&r, garruk, -4);

    let check = |r: &GameRunner| {
        let gates = ActivationRestrictionStaticGates::compute(r.state());
        perf_counters::reset();
        let result = check_activation_restrictions_with_static_gates(
            r.state(),
            P0,
            garruk,
            minus_four,
            &[ActivationRestriction::AsSorcery],
            &gates,
        );
        (result.is_ok(), perf_counters::snapshot())
    };

    set_window(&mut r, P0, Phase::PreCombatMain);
    let (legal, counters) = check(&r);
    assert!(legal, "own main");
    assert_eq!(counters.restriction_static_mode_gate_scans, 0);
    assert_eq!(counters.restriction_static_exact_scans, 0);

    set_window(&mut r, P0, Phase::Upkeep);
    let (legal, counters) = check(&r);
    assert!(!legal, "own upkeep");
    assert_eq!(counters.restriction_static_mode_gate_scans, 0);
    assert_eq!(
        counters.restriction_static_exact_scans, 1,
        "reach: the permission scan runs outside the window"
    );
}

/// PF-2. Decision 2.5: an instant reaches no flash scan, and an ability computes
/// gates only on reaching an `AsSorcery` restriction. Not a rules reading.
#[test]
fn queries_reach_no_static_scan_for_instants_and_untimed_abilities() {
    let mut s = scenario();
    add_leyline(&mut s, P0);
    let emperor = add_emperor(&mut s);
    let garruk = add_garruk(&mut s);
    let shivan = s
        .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, SHIVAN_DRAGON)
        .id();
    let azimaet = s
        .add_creature_from_oracle(P0, "Azimaet Drake", 1, 3, AZIMAET_DRAKE)
        .id();
    let giant_growth = s
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
        .id();
    let overrun = add_overrun(&mut s);
    let mut r = s.build();
    set_emperor_entered(&mut r, emperor, true);
    set_window(&mut r, P0, Phase::PreCombatMain);

    perf_counters::reset();
    assert!(!spell_query(&r, giant_growth));
    assert_eq!(perf_counters::snapshot().spell_keyword_grant_scans, 0);

    perf_counters::reset();
    assert!(!spell_query(&r, overrun));
    assert!(
        perf_counters::snapshot().spell_keyword_grant_scans >= 1,
        "reach: a sorcery reaches the flash scan"
    );

    let shivan_index = only_activated_index(&r, shivan);
    let azimaet_index = only_activated_index(&r, azimaet);
    perf_counters::reset();
    assert!(!ability_query(&r, shivan, shivan_index));
    assert!(!ability_query(&r, azimaet, azimaet_index));
    assert_eq!(
        perf_counters::snapshot().restriction_static_mode_gate_scans,
        0
    );

    let minus_four = loyalty_index(&r, garruk, -4);
    perf_counters::reset();
    assert!(ability_query(&r, garruk, minus_four));
    assert_eq!(
        perf_counters::snapshot().restriction_static_mode_gate_scans,
        1,
        "reach: an AsSorcery restriction computes the gates once"
    );
}
