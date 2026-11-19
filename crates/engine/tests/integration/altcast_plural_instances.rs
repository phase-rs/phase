//! Part of issue #10957: a graveyard card with two distinct instances of one
//! alternative-cast keyword (a printed flashback and Lier's granted one; a
//! printed escape and Underworld Breach's) offers one cast per instance, and
//! the cast pays the instance the caster chose.
//!
//! CR 113.2c: "If an object has multiple instances of the same ability, each
//! instance functions independently." CR 118.9a + CR 601.2b: one alternative
//! cost applies, and the caster announces which. Rulings: "If a card has
//! multiple instances of flashback, you may choose any of its flashback costs
//! to pay." / "If a card has multiple abilities giving you permission to cast
//! it, such as two escape abilities …, you choose which one to apply."
//!
//! Verbatim Oracle text (Scryfall, 2026-11-16) for every fixture below.

use engine::ai_support::legal_actions;
use engine::game::casting::legal_target_slots_for_castable_spell;
use engine::game::combat::AttackTarget;
use engine::game::keywords::effective_keyword_instances;
use engine::game::scenario::{GameRunner, GameScenario, OptionalPolicy, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCost, AdditionalCostOrigin, ModalSelectionCondition, ModalSelectionConstraint, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::{CastingVariant, CastingVariantChoiceOption, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{FlashbackCost, Keyword, KeywordKind};
use engine::types::mana::{ManaCost, ManaCostShard, ManaRestriction, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const LIER: &str = "Spells can't be countered.\nEach instant and sorcery card in your graveyard has flashback. The flashback cost is equal to that card's mana cost.";
const DEEP_ANALYSIS: &str = "Target player draws two cards.\nFlashback—{1}{U}, Pay 3 life. (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const BREACH: &str = "Each nonland card in your graveyard has escape. The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard. (You may cast cards from your graveyard for their escape cost.)\nAt the beginning of the end step, sacrifice this enchantment.";
const URO: &str = "When Uro enters, sacrifice it unless it escaped.\nWhenever Uro enters or attacks, you gain 3 life and draw a card, then you may put a land card from your hand onto the battlefield.\nEscape—{G}{G}{U}{U}, Exile five other cards from your graveyard. (You may cast this card from your graveyard for its escape cost.)";
const RETURN_THE_PAST: &str = "During your turn, each instant and sorcery card in your graveyard has flashback. Its flashback cost is equal to its mana cost.";
const DIVINATION: &str = "Draw two cards.";
const COFFIN_PURGE: &str = "Exile target card from a graveyard.\nFlashback {B} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const MURDER: &str = "Destroy target creature.";
const NATURALIZE: &str = "Destroy target artifact or enchantment.";

fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { generic, shards }
}

const SPARE_SOURCE: ObjectId = ObjectId(8_888);

/// Mana units for the color letters in `colors` (W U B R G C), plus an
/// identifiable spare that can't pay a spell (activation-only), so it must
/// survive every cast below.
fn units(colors: &str) -> Vec<ManaUnit> {
    let mut units: Vec<_> = colors
        .chars()
        .map(|letter| {
            let color = match letter {
                'W' => ManaType::White,
                'U' => ManaType::Blue,
                'B' => ManaType::Black,
                'R' => ManaType::Red,
                'G' => ManaType::Green,
                'C' => ManaType::Colorless,
                other => panic!("no mana letter {other}"),
            };
            ManaUnit::new(color, ObjectId(9_999), false, vec![])
        })
        .collect();
    let spare = vec![ManaRestriction::OnlyForActivation];
    units.push(ManaUnit::new(ManaType::Red, SPARE_SOURCE, false, spare));
    units
}

fn pool(s: &mut GameScenario, player: PlayerId, colors: &str) {
    s.with_mana_pool(player, units(colors));
}

/// [`pool`] for P0 mid-game (the pool empties as steps end).
fn fund(r: &mut GameRunner, colors: &str) {
    for unit in units(colors) {
        let _ = r.state_mut().add_mana_to_pool(P0, unit);
    }
}

fn pool_colors(r: &GameRunner) -> Vec<(ManaType, ObjectId)> {
    let mut units: Vec<_> = r.state().players[0]
        .mana_pool
        .units()
        .map(|unit| (unit.color, unit.source_id))
        .collect();
    units.sort_by_key(|(color, source)| (format!("{color:?}"), source.0));
    units
}

/// `left` units remain, the spare among them and none of color `spent`, and
/// `card` was exiled.
fn assert_paid(r: &GameRunner, card: ObjectId, left: usize, spent: Option<ManaType>) {
    let pool = pool_colors(r);
    assert_eq!(pool.len(), left, "{pool:?}");
    assert!(pool.contains(&(ManaType::Red, SPARE_SOURCE)), "{pool:?}");
    assert!(
        spent.is_none_or(|spent| pool.iter().all(|(color, _)| *color != spent)),
        "{pool:?}"
    );
    assert_eq!(r.state().objects[&card].zone, Zone::Exile);
}

fn lier(s: &mut GameScenario) -> ObjectId {
    s.add_creature(P0, "Lier, Disciple of the Drowned", 3, 4)
        .as_legendary()
        .with_subtypes(vec!["Human", "Wizard"])
        .with_mana_cost(cost(3, vec![ManaCostShard::Blue, ManaCostShard::Blue]))
        .from_oracle_text(LIER)
        .id()
}

fn deep_analysis(s: &mut GameScenario) -> ObjectId {
    s.add_spell_to_graveyard(P0, "Deep Analysis", false)
        .with_mana_cost(cost(3, vec![ManaCostShard::Blue]))
        .from_oracle_text(DEEP_ANALYSIS)
        .id()
}

fn library(s: &mut GameScenario, n: usize) {
    for i in 0..n {
        s.add_card_to_library_top(P0, &format!("Library {i}"));
    }
}

fn casts_offered(r: &GameRunner, card: ObjectId) -> usize {
    legal_actions(r.state())
        .into_iter()
        .filter(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == card))
        .count()
}

fn start_cast(r: &mut GameRunner, card: ObjectId) {
    let card_id = r.state().objects[&card].card_id;
    let cast = GameAction::CastSpell {
        object_id: card,
        card_id,
        targets: vec![],
        payment_mode: Default::default(),
    };
    r.act(cast).expect("the cast is accepted");
}

/// The options of the menu a `CastSpell` of `card` opens, or empty when the
/// cast goes on without one. Read on a clone, so the board is unchanged.
fn menu(r: &GameRunner, card: ObjectId) -> Vec<CastingVariantChoiceOption> {
    let mut probe = GameRunner::from_state(r.state().clone());
    start_cast(&mut probe, card);
    match &probe.state().waiting_for {
        WaitingFor::CastingVariantChoice { options, .. } => options.clone(),
        _ => Vec::new(),
    }
}

fn instances(r: &GameRunner, card: ObjectId, kind: KeywordKind) -> Vec<Keyword> {
    effective_keyword_instances(r.state(), card, kind)
}

fn assert_settled(r: &GameRunner) {
    assert!(r.state().stack.is_empty(), "the stack is empty");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "priority returns, got {:?}",
        r.state().waiting_for
    );
}

fn lier_board(life: i32, blues: usize) -> (GameRunner, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    lier(&mut s);
    let da = deep_analysis(&mut s);
    library(&mut s, 3);
    pool(&mut s, P0, &"U".repeat(blues));
    let mut r = s.build();
    r.state_mut().players[0].life = life;
    (r, da)
}

/// {U}{U}: only the printed flashback is payable; before #10957 the printed
/// instance was lost to Lier's and nothing was offered.
#[test]
fn printed_flashback_stays_castable_beside_lier() {
    let (mut r, da) = lier_board(20, 2);
    assert_eq!(instances(&r, da, KeywordKind::Flashback).len(), 2);
    assert_eq!(casts_offered(&r, da), 1);
    assert!(menu(&r, da).is_empty(), "one payable option: no menu");
    r.cast(da).target_player(P0).resolve();
    assert_eq!(r.life(P0), 17, "{{1}}{{U}} and 3 life");
    assert_eq!(r.state().players[0].hand.len(), 2);
    assert_paid(&r, da, 1, None);
    assert_settled(&r);
}

/// {U}x4: two alternative costs, one per instance; each pays its own cost.
#[test]
fn each_flashback_instance_is_its_own_offer() {
    let (r, da) = lier_board(20, 4);
    let options = menu(&r, da);
    assert_eq!(options.len(), 2, "{options:?}");
    let costs: Vec<_> = options.iter().map(|o| o.mana_cost.clone()).collect();
    assert!(costs.contains(&cost(1, vec![ManaCostShard::Blue])));
    assert!(costs.contains(&cost(3, vec![ManaCostShard::Blue])));
    let [printed, granted] = <[Keyword; 2]>::try_from(instances(&r, da, KeywordKind::Flashback))
        .expect("two flashback instances");
    for (keyword, life, spare_blues) in [(printed, 17, 2), (granted, 20, 0)] {
        let (mut r, da) = lier_board(20, 4);
        r.cast(da)
            .casting_variant(CastingVariant::Flashback)
            .casting_keyword(keyword)
            .target_player(P0)
            .resolve();
        assert_eq!(r.life(P0), life);
        let mut expected = vec![(ManaType::Blue, ObjectId(9_999)); spare_blues];
        expected.push((ManaType::Red, SPARE_SOURCE));
        assert_eq!(pool_colors(&r), expected);
        assert_eq!(r.state().objects[&da].zone, Zone::Exile);
        assert_settled(&r);
    }
}

/// At 2 life the printed instance's "Pay 3 life" can't be paid (CR 119.4):
/// only Lier's instance is offered, and it charges no life. Its targets
/// preview (CR 601.2c) as the cast will announce them; with two options the
/// preview waits for the choice.
#[test]
fn unpayable_life_leaves_only_the_granted_flashback() {
    let (mut r, da) = lier_board(2, 4);
    assert!(menu(&r, da).is_empty(), "only Lier's option: no menu");
    assert_eq!(casts_offered(&r, da), 1);
    let slots = legal_target_slots_for_castable_spell(r.state(), da);
    assert_eq!(slots.len(), 1, "{slots:?}");
    let legal = &slots[0].legal_targets;
    assert!(
        legal.contains(&player(P0)) && legal.contains(&player(P1)),
        "{legal:?}"
    );
    r.cast(da).target_player(P0).resolve();
    assert_eq!(r.life(P0), 2);
    assert_eq!(r.state().players[0].hand.len(), 2, "drew two");
    assert_paid(&r, da, 1, None);
    assert_settled(&r);
    let (r, da) = lier_board(4, 4);
    assert_eq!(menu(&r, da).len(), 2, "life-payable control");
    assert!(legal_target_slots_for_castable_spell(r.state(), da).is_empty());
}

fn uro_board(breach: bool, others: usize, colors: &str) -> (GameRunner, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    if breach {
        s.add_enchantment_from_oracle(P0, "Underworld Breach", BREACH);
    }
    let uro = s
        .add_creature_to_graveyard(P0, "Uro, Titan of Nature's Wrath", 6, 6)
        .as_legendary()
        .with_subtypes(vec!["Elder", "Giant"])
        .with_mana_cost(cost(1, vec![ManaCostShard::Green, ManaCostShard::Blue]))
        .from_oracle_text(URO)
        .id();
    let names: Vec<String> = (0..others).map(|i| format!("Other {i}")).collect();
    s.with_graveyard(P0, &names.iter().map(String::as_str).collect::<Vec<_>>());
    library(&mut s, 2);
    pool(&mut s, P0, colors);
    (s.build(), uro)
}

fn graveyard_len(r: &GameRunner) -> usize {
    r.state().players[0].graveyard.len()
}

/// The first `n` graveyard cards other than `card`, to pay an escape exile.
fn others(r: &GameRunner, card: ObjectId, n: usize) -> Vec<ObjectId> {
    let graveyard = &r.state().players[0].graveyard;
    graveyard
        .iter()
        .copied()
        .filter(|id| *id != card)
        .take(n)
        .collect()
}

/// Five others and {G}{G}{U}{U}: Uro's printed escape and Breach's both offer;
/// each exiles exactly its own count, and Uro stays (it escaped, CR 702.138b).
#[test]
fn each_escape_instance_is_its_own_offer() {
    let (r, uro) = uro_board(true, 5, "GGUU");
    let options = menu(&r, uro);
    assert_eq!(options.len(), 2, "{options:?}");
    let [printed, granted] = <[Keyword; 2]>::try_from(instances(&r, uro, KeywordKind::Escape))
        .expect("two escape instances");
    let gguu = vec![
        ManaCostShard::Green,
        ManaCostShard::Green,
        ManaCostShard::Blue,
        ManaCostShard::Blue,
    ];
    let breach = cost(1, vec![ManaCostShard::Green, ManaCostShard::Blue]);
    for (keyword, exiled, mana, left) in [(printed, 5, cost(0, gguu), 1), (granted, 3, breach, 2)] {
        let option = options
            .iter()
            .find(|o| o.alternative_cost_keyword.as_ref() == Some(&keyword));
        let option = option.expect("each instance is offered");
        assert_eq!(option.mana_cost, mana, "its own mana");
        let exile = matches!(option.additional_cost, Some(AbilityCost::Exile { count, .. }) if count as usize == exiled);
        assert!(exile, "its own exile count: {option:?}");
        let (mut r, uro) = uro_board(true, 5, "GGUU");
        let before = graveyard_len(&r);
        let fodder = others(&r, uro, exiled);
        r.cast(uro)
            .casting_variant(CastingVariant::Escape)
            .casting_keyword(keyword)
            .pay_cost_with(&fodder)
            .decline_optional()
            .resolve();
        assert_eq!(before - 1 - graveyard_len(&r), exiled);
        assert_eq!(r.state().objects[&uro].zone, Zone::Battlefield, "escaped");
        let pool = pool_colors(&r);
        assert_eq!(pool.len(), left, "{pool:?}");
        assert!(pool.contains(&(ManaType::Red, SPARE_SOURCE)));
        assert_settled(&r);
    }
}

/// Three others and {G}{G}{U}{U}: both mana parts are affordable, but only
/// Breach's exile-three can be paid, so it alone is offered.
#[test]
fn escape_options_intersect_mana_and_exile_on_the_same_instance() {
    let (mut r, uro) = uro_board(true, 3, "GGUU");
    assert!(menu(&r, uro).is_empty(), "one option: no menu");
    assert_eq!(casts_offered(&r, uro), 1);
    let fodder = others(&r, uro, 3);
    r.cast(uro)
        .pay_cost_with(&fodder)
        .decline_optional()
        .resolve();
    assert_eq!(graveyard_len(&r), 0, "three others exiled");
    let left = pool_colors(&r);
    assert_eq!(
        left.len(),
        2,
        "{{1}}{{G}}{{U}} paid: one G-or-U and the spare"
    );
    assert!(left.contains(&(ManaType::Red, SPARE_SOURCE)));
    assert!(left
        .iter()
        .any(|(c, s)| *s != SPARE_SOURCE && matches!(c, ManaType::Green | ManaType::Blue)));
    assert_eq!(r.state().objects[&uro].zone, Zone::Battlefield);
    assert_settled(&r);
    // Completion with exactly Breach's mana.
    let (mut r, uro) = uro_board(true, 3, "CGU");
    let fodder = others(&r, uro, 3);
    r.cast(uro)
        .pay_cost_with(&fodder)
        .decline_optional()
        .resolve();
    assert_eq!(pool_colors(&r), vec![(ManaType::Red, SPARE_SOURCE)]);
    assert_eq!(r.state().objects[&uro].zone, Zone::Battlefield);
    assert_settled(&r);
}

/// Printed-only control: no Breach, five others, Uro's own escape.
#[test]
fn printed_escape_alone_pays_its_own_cost() {
    let (mut r, uro) = uro_board(false, 5, "GGUU");
    let fodder = others(&r, uro, 5);
    r.cast(uro)
        .pay_cost_with(&fodder)
        .decline_optional()
        .resolve();
    assert_eq!(graveyard_len(&r), 0, "five others exiled");
    assert_eq!(pool_colors(&r), vec![(ManaType::Red, SPARE_SOURCE)]);
    assert_eq!(r.state().objects[&uro].zone, Zone::Battlefield);
    assert_settled(&r);
}

/// Lier and Return the Past grant the same "flashback equal to its mana cost":
/// one instance, one offer.
#[test]
fn identical_grants_are_one_offer() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    lier(&mut s);
    s.add_enchantment_from_oracle(P0, "Return the Past", RETURN_THE_PAST);
    let divination = s
        .add_spell_to_graveyard(P0, "Divination", false)
        .with_mana_cost(cost(2, vec![ManaCostShard::Blue]))
        .from_oracle_text(DIVINATION)
        .id();
    library(&mut s, 2);
    pool(&mut s, P0, "UUU");
    let mut r = s.build();
    assert_eq!(instances(&r, divination, KeywordKind::Flashback).len(), 1);
    assert!(menu(&r, divination).is_empty());
    r.cast(divination).resolve();
    assert_paid(&r, divination, 1, None);
    assert_settled(&r);
}

/// Coffin Purge's printed {B} and Lier's "its mana cost" ({B}) are distinct
/// instances with one total: one option (CR 601.2b).
#[test]
fn instances_with_the_same_total_are_one_option() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    lier(&mut s);
    let purge = s
        .add_spell_to_graveyard(P0, "Coffin Purge", true)
        .with_mana_cost(cost(0, vec![ManaCostShard::Black]))
        .from_oracle_text(COFFIN_PURGE)
        .id();
    let target = s.add_creature_to_graveyard(P1, "Grave Target", 2, 2).id();
    pool(&mut s, P0, "B");
    let mut r = s.build();
    assert_eq!(instances(&r, purge, KeywordKind::Flashback).len(), 2);
    assert_eq!(casts_offered(&r, purge), 1);
    assert!(
        menu(&r, purge).is_empty(),
        "two instances, one total: no menu"
    );
    r.cast(purge).target_object(target).resolve();
    assert_eq!(r.state().objects[&target].zone, Zone::Exile);
    assert_paid(&r, purge, 1, None);
    assert_settled(&r);
}

/// Lier dies in response to Deep Analysis cast for Lier's cost: it still
/// resolves and is still exiled (CR 702.34a keys on the cast method).
#[test]
fn granted_flashback_survives_its_grantor_leaving() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let lier = lier(&mut s);
    let da = deep_analysis(&mut s);
    let murder = s
        .add_spell_to_hand(P1, "Murder", true)
        .with_mana_cost(cost(1, vec![ManaCostShard::Black, ManaCostShard::Black]))
        .from_oracle_text(MURDER)
        .id();
    library(&mut s, 3);
    pool(&mut s, P0, "UUUU");
    pool(&mut s, P1, "BBB");
    let mut r = s.build();
    let granted = instances(&r, da, KeywordKind::Flashback)[1].clone();
    let mut commit = r
        .cast(da)
        .casting_variant(CastingVariant::Flashback)
        .casting_keyword(granted)
        .target_player(P0)
        .commit();
    commit.act(GameAction::PassPriority).expect("P0 passes");
    commit.cast(murder).target_object(lier).commit().resolve();
    assert_eq!(r.state().objects[&lier].zone, Zone::Graveyard);
    r.advance_until_stack_empty();
    assert_eq!(r.state().players[0].hand.len(), 2, "Deep Analysis resolved");
    assert_eq!(r.state().objects[&da].zone, Zone::Exile);
    assert_eq!(r.life(P0), 20);
    assert_settled(&r);
}

/// Breach is destroyed in response to Uro cast for Breach's escape: Uro still
/// escaped and stays.
#[test]
fn granted_escape_survives_its_grantor_leaving() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let breach = s
        .add_enchantment_from_oracle(P0, "Underworld Breach", BREACH)
        .id();
    let uro = s
        .add_creature_to_graveyard(P0, "Uro, Titan of Nature's Wrath", 6, 6)
        .as_legendary()
        .with_mana_cost(cost(1, vec![ManaCostShard::Green, ManaCostShard::Blue]))
        .from_oracle_text(URO)
        .id();
    s.with_graveyard(P0, &["Other 0", "Other 1", "Other 2", "Other 3", "Other 4"]);
    let naturalize = s
        .add_spell_to_hand(P1, "Naturalize", true)
        .with_mana_cost(cost(1, vec![ManaCostShard::Green]))
        .from_oracle_text(NATURALIZE)
        .id();
    library(&mut s, 2);
    pool(&mut s, P0, "GGUU");
    pool(&mut s, P1, "GG");
    let mut r = s.build();
    let granted = instances(&r, uro, KeywordKind::Escape)[1].clone();
    let fodder = others(&r, uro, 3);
    let mut commit = r
        .cast(uro)
        .casting_variant(CastingVariant::Escape)
        .casting_keyword(granted)
        .pay_cost_with(&fodder)
        .decline_optional()
        .commit();
    commit.act(GameAction::PassPriority).expect("P0 passes");
    commit
        .cast(naturalize)
        .target_object(breach)
        .commit()
        .resolve();
    r.advance_until_stack_empty();
    assert_eq!(r.state().objects[&breach].zone, Zone::Graveyard);
    assert_eq!(r.state().objects[&uro].zone, Zone::Battlefield, "escaped");
    assert_settled(&r);
}

// ── The Fugitive Doctor grants a second, distinct flashback ──

const DOCTOR: &str = "When The Fugitive Doctor enters, investigate.\nWhenever The Fugitive Doctor attacks, you may sacrifice a Clue. When you do, target instant or sorcery card in your graveyard gains flashback {2}{R}{G} until end of turn. (You may cast that card from your graveyard for its flashback cost. Then exile it.)";
/// CR 111.10f: "A Clue token is a colorless Clue artifact token with '{2},
/// Sacrifice this token: Draw a card.'"
const CLUE: &str = "{2}, Sacrifice this token: Draw a card.";

/// Lier + The Fugitive Doctor + a Clue token (the board after the Doctor's
/// "investigate", which scenario seeding doesn't fire), then a real attack:
/// the Doctor's trigger sacrifices the Clue and its reflexive trigger grants
/// `card` flashback {2}{R}{G}. Stops at P0's postcombat main.
fn doctor_grant(
    s: &mut GameScenario,
    card: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId) {
    s.at_phase(Phase::PreCombatMain);
    lier(s);
    let doctor = s
        .add_creature(P0, "The Fugitive Doctor", 4, 4)
        .as_legendary()
        .with_subtypes(vec!["Time Lord", "Doctor"])
        .from_oracle_text(DOCTOR)
        .id();
    let clue = s
        .add_artifact_from_oracle(P0, "Clue", CLUE)
        .with_subtypes(vec!["Clue"])
        .id();
    let card = card(s);
    let mut r = std::mem::take(s).build();
    r.state_mut().objects.get_mut(&clue).expect("clue").is_token = true;
    assert_eq!(r.state().objects[&clue].zone, Zone::Battlefield, "a Clue");
    r.advance_to_combat();
    r.declare_attackers(&[(doctor, engine::game::combat::AttackTarget::Player(P1))])
        .expect("the Doctor attacks");
    for _ in 0..40 {
        let action = match &r.state().waiting_for {
            WaitingFor::Priority { .. }
                if r.state().phase == Phase::PostCombatMain && r.state().stack.is_empty() =>
            {
                break
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::PayCost { .. } => GameAction::SelectCards { cards: vec![clue] },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::TriggerTargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(card)),
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        r.act(action).expect("the attack step is accepted");
    }
    let clue = r.state().objects.get(&clue);
    assert!(
        clue.is_none_or(|clue| clue.zone != Zone::Battlefield),
        "sacrificed"
    );
    let flashbacks = instances(&r, card, KeywordKind::Flashback);
    assert!(flashbacks.contains(&lier_flashback()), "{flashbacks:?}");
    assert!(flashbacks.contains(&doctor_flashback()), "{flashbacks:?}");
    (r, card)
}

fn lier_flashback() -> Keyword {
    Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost))
}

fn doctor_flashback() -> Keyword {
    let mana = cost(2, vec![ManaCostShard::Red, ManaCostShard::Green]);
    Keyword::Flashback(FlashbackCost::Mana(mana))
}

/// Casts `card` electing `keyword` at its menu and returns the first prompt
/// after the menu, whose pending cast must carry the elected instance.
fn elect(r: &mut GameRunner, card: ObjectId, keyword: &Keyword) -> WaitingFor {
    start_cast(r, card);
    let WaitingFor::CastingVariantChoice { options, .. } = r.state().waiting_for.clone() else {
        panic!("a casting menu, got {:?}", r.state().waiting_for);
    };
    let index = options
        .iter()
        .position(|option| option.alternative_cost_keyword.as_ref() == Some(keyword))
        .expect("the elected instance is offered");
    r.act(GameAction::ChooseCastingVariant { index })
        .expect("the option is chosen");
    let paused = r.state().waiting_for.clone();
    let carried = paused
        .pending_cast_ref()
        .and_then(|pending| pending.alternative_cost_keyword.as_ref());
    assert_eq!(
        carried,
        Some(keyword),
        "the paused cast carries it: {paused:?}"
    );
    paused
}

/// How [`finish`] answers the announcement: each target slot gets the next of
/// `targets`; optional costs follow `costs`.
#[derive(Default)]
struct Answers {
    costs: OptionalPolicy,
    targets: Vec<TargetRef>,
    x: u32,
    modes: Vec<usize>,
}

fn aim(targets: &[TargetRef]) -> Answers {
    let targets = targets.to_vec();
    Answers {
        targets,
        ..Answers::default()
    }
}

impl Answers {
    fn x(self, x: u32) -> Self {
        Self { x, ..self }
    }

    fn modes(self, modes: &[usize]) -> Self {
        let modes = modes.to_vec();
        Self { modes, ..self }
    }

    fn paying(self) -> Self {
        let costs = OptionalPolicy::Accept;
        Self { costs, ..self }
    }
}

/// Answers the rest of the announcement, asserting that every paused cast
/// still carries `keyword`, then resolves the stack.
fn finish(r: &mut GameRunner, keyword: &Keyword, answers: Answers) {
    let mut targets = answers.targets.into_iter();
    for _ in 0..40 {
        if let Some(pending) = r.state().waiting_for.pending_cast_ref() {
            let carried = pending.alternative_cost_keyword.as_ref();
            assert_eq!(carried, Some(keyword), "{:?}", r.state().waiting_for);
        }
        let action = match &r.state().waiting_for {
            WaitingFor::Priority { .. } => break,
            WaitingFor::OptionalCostChoice { .. } => match answers.costs {
                OptionalPolicy::Accept => GameAction::DecideOptionalCost { index: 0 },
                OptionalPolicy::Decline => GameAction::DecideOptionalCost { index: 1 },
                OptionalPolicy::Stop => break,
            },
            WaitingFor::SpliceOffer { .. } => GameAction::RespondToSpliceOffer { card: None },
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: answers.x },
            WaitingFor::ModeChoice { .. } => GameAction::SelectModes {
                indices: answers.modes.clone(),
            },
            WaitingFor::ChooseAnnouncingOpponent { candidates, .. } => {
                GameAction::ChooseAnnouncingOpponent {
                    opponent: candidates[0],
                }
            }
            WaitingFor::ChooseGiftRecipient { candidates, .. } => GameAction::ChooseGiftRecipient {
                opponent: candidates[0],
            },
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: targets.next(),
            },
            WaitingFor::DistributeAmong { total, targets, .. } => GameAction::DistributeAmong {
                distribution: vec![(targets[0].clone(), *total)],
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        r.act(action).expect("the announcement step is accepted");
    }
    r.advance_until_stack_empty();
    assert_settled(r);
}

/// Elects `keyword` for `card`, checks the first prompt after the menu with
/// `paused` (the pause class under test), then answers and resolves.
fn cast_row(
    r: &mut GameRunner,
    card: ObjectId,
    keyword: &Keyword,
    paused: impl Fn(&WaitingFor) -> bool,
    answers: Answers,
) {
    let prompt = elect(r, card, keyword);
    assert!(paused(&prompt), "{prompt:?}");
    finish(r, keyword, answers);
}

/// `name` in P0's graveyard: an instant when `instant`, else a sorcery.
fn graveyard_card(
    s: &mut GameScenario,
    (name, instant): (&str, bool),
    mana: ManaCost,
    keywords: &[&str],
    text: &str,
) -> ObjectId {
    s.add_spell_to_graveyard(P0, name, instant)
        .with_mana_cost(mana)
        .from_oracle_text_with_keywords(keywords, text)
        .id()
}

fn player(id: PlayerId) -> TargetRef {
    TargetRef::Player(id)
}

fn object(id: ObjectId) -> TargetRef {
    TargetRef::Object(id)
}

const ANHELO: &str = "Deathtouch\nThe first instant or sorcery spell you cast each turn has casualty 2. (As you cast that spell, you may sacrifice a creature with power 2 or greater. When you do, copy the spell and you may choose new targets for the copy.)";
const DJINN: &str = "Flying\nEach instant and sorcery spell you cast has replicate. The replicate cost is equal to its mana cost. (When you cast it, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)";

/// CR 601.2b: casualty is declared before targets (the keyword queue).
#[test]
fn casualty_queue_pause_keeps_the_elected_flashback() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    lier(&mut s);
    s.add_creature(P0, "Anhelo, the Painter", 1, 3)
        .as_legendary()
        .from_oracle_text(ANHELO);
    s.add_vanilla(P0, 2, 2);
    let da = deep_analysis(&mut s);
    library(&mut s, 3);
    pool(&mut s, P0, "UUUU");
    let mut r = s.build();
    let printed = instances(&r, da, KeywordKind::Flashback)[0].clone();
    let queue = |w: &WaitingFor| {
        matches!(w, WaitingFor::OptionalCostChoice { pending_cast, .. }
            if pending_cast.keyword_queue_before_targets)
    };
    cast_row(&mut r, da, &printed, queue, aim(&[player(P0)]));
    assert_eq!(r.life(P0), 17);
    assert_eq!(r.state().players[0].hand.len(), 2);
    assert_paid(&r, da, 3, None);
}

/// CR 702.56a: replicate is an optional cost declared before targets.
#[test]
fn replicate_pause_keeps_the_elected_flashback() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    lier(&mut s);
    s.add_creature(P0, "Djinn Illuminatus", 3, 5)
        .from_oracle_text(DJINN);
    let da = deep_analysis(&mut s);
    library(&mut s, 3);
    pool(&mut s, P0, "UUUU");
    let mut r = s.build();
    let deferred = |w: &WaitingFor| {
        matches!(w, WaitingFor::OptionalCostChoice { pending_cast, .. }
            if pending_cast.deferred_target_selection)
    };
    cast_row(&mut r, da, &lier_flashback(), deferred, aim(&[player(P0)]));
    assert_eq!(r.life(P0), 20, "Lier's instance charges no life");
    assert_paid(&r, da, 1, None);
}

/// CR 702.47a: splice is offered before targets.
#[test]
fn splice_offer_keeps_the_elected_flashback() {
    let mut s = GameScenario::new();
    s.add_spell_to_hand(P0, "Desperate Ritual", true)
        .with_mana_cost(cost(1, vec![ManaCostShard::Red]))
        .with_subtypes(vec!["Arcane"])
        .from_oracle_text("Add {R}{R}{R}.\nSplice onto Arcane {1}{R} (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)");
    let (mut r, spike) = doctor_grant(&mut s, |s| {
        s.add_spell_to_graveyard(P0, "Lava Spike", false)
            .with_mana_cost(cost(0, vec![ManaCostShard::Red]))
            .with_subtypes(vec!["Arcane"])
            .from_oracle_text("Lava Spike deals 3 damage to target player or planeswalker.")
            .id()
    });
    fund(&mut r, "CCRG");
    let life = r.life(P1);
    let splice = |w: &WaitingFor| matches!(w, WaitingFor::SpliceOffer { .. });
    cast_row(
        &mut r,
        spike,
        &doctor_flashback(),
        splice,
        aim(&[player(P1)]),
    );
    assert_eq!(r.life(P1), life - 3);
    assert_paid(&r, spike, 1, None);
}

fn deferred_x(w: &WaitingFor) -> bool {
    matches!(w, WaitingFor::ChooseXValue { pending_cast, .. }
        if pending_cast.deferred_target_selection)
}

/// CR 601.2b + CR 107.3a: X is announced before the X-bounded targets.
#[test]
fn x_before_targets_keeps_the_elected_flashback() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    lier(&mut s);
    let db = crate::support::shared_card_db().expect("the integration card database");
    let conflagrate = s.add_real_card(P0, "Conflagrate", Zone::Graveyard, db);
    let keep = s.add_card_to_hand(P0, "Keep");
    pool(&mut s, P0, "RRR");
    let mut r = s.build();
    let printed = instances(&r, conflagrate, KeywordKind::Flashback);
    assert_eq!(
        printed.len(),
        2,
        "the printed and Lier's flashback: {printed:?}"
    );
    let answers = aim(&[player(P1)]).x(1);
    cast_row(&mut r, conflagrate, &lier_flashback(), deferred_x, answers);
    assert_eq!(r.life(P1), 19);
    assert_eq!(r.state().objects[&keep].zone, Zone::Hand, "no discard");
    assert_paid(&r, conflagrate, 1, None);
}

/// CR 601.2b: a required additional cost with X is announced before targets.
#[test]
fn required_x_cost_keeps_the_elected_flashback() {
    let mut s = GameScenario::new();
    let bear = s.add_vanilla(P1, 2, 2);
    let (mut r, covenant) = doctor_grant(&mut s, |s| {
        let mana = cost(1, vec![ManaCostShard::Black, ManaCostShard::Red]);
        graveyard_card(s, ("Fire Covenant", true), mana, &[], "As an additional cost to cast this spell, pay X life.\nFire Covenant deals X damage divided as you choose among any number of target creatures.")
    });
    fund(&mut r, "CCRGB");
    let life = r.life(P0);
    let answers = aim(&[object(bear)]).x(2);
    cast_row(&mut r, covenant, &doctor_flashback(), deferred_x, answers);
    assert_eq!(r.life(P0), life - 2);
    assert_eq!(r.state().objects[&bear].zone, Zone::Graveyard);
    assert_paid(&r, covenant, 2, None);
}

/// CR 601.2b: a kicker that changes the target is declared before targets.
#[test]
fn target_changing_kicker_keeps_the_elected_flashback() {
    let mut s = GameScenario::new();
    let black = cost(2, vec![ManaCostShard::Black]);
    let ogre = s
        .add_creature(P1, "Three-Drop", 3, 3)
        .with_mana_cost(black)
        .id();
    let (mut r, thirst) = doctor_grant(&mut s, |s| {
        let mana = cost(0, vec![ManaCostShard::Black]);
        graveyard_card(s, ("Bloodchief's Thirst", false), mana, &["Kicker"], "Kicker {2}{B} (You may pay an additional {2}{B} as you cast this spell.)\nDestroy target creature or planeswalker with mana value 2 or less. If this spell was kicked, instead destroy target creature or planeswalker.")
    });
    fund(&mut r, "CCBBRG");
    let deferred = |w: &WaitingFor| {
        matches!(w, WaitingFor::OptionalCostChoice { pending_cast, .. }
            if pending_cast.deferred_target_selection)
    };
    let answers = aim(&[object(ogre)]).paying();
    cast_row(&mut r, thirst, &lier_flashback(), deferred, answers);
    assert_eq!(r.state().objects[&ogre].zone, Zone::Graveyard);
    assert_paid(&r, thirst, 3, Some(ManaType::Black));
}

const RELIC: &str = "{T}: You gain 1 life.";

/// CR 702.174a: a gift is promised before targets.
#[test]
fn gift_queue_keeps_the_elected_flashback() {
    let mut s = GameScenario::new();
    let relic = s.add_artifact_from_oracle(P1, "Relic", RELIC).id();
    let (mut r, maw) = doctor_grant(&mut s, |s| {
        let mana = cost(0, vec![ManaCostShard::Blue]);
        graveyard_card(s, ("Into the Flood Maw", true), mana, &["Gift"], "Gift a tapped Fish (You may promise an opponent a gift as you cast this spell. If you do, they create a tapped 1/1 blue Fish creature token before its other effects.)\nReturn target creature an opponent controls to its owner's hand. If the gift was promised, instead return target nonland permanent an opponent controls to its owner's hand.")
    });
    fund(&mut r, "UCCRG");
    let gift = |w: &WaitingFor| matches!(w, WaitingFor::OptionalCostChoice { .. });
    cast_row(
        &mut r,
        maw,
        &lier_flashback(),
        gift,
        aim(&[object(relic)]).paying(),
    );
    assert_eq!(r.state().objects[&relic].zone, Zone::Hand);
    let state = r.state();
    let p1_permanents = state
        .battlefield
        .iter()
        .filter(|id| state.objects[*id].controller == P1);
    assert_eq!(p1_permanents.count(), 1, "the gift's Fish");
    assert_paid(&r, maw, 5, Some(ManaType::Blue));
}

const WRECK_AND_REBUILD: &str = "Choose one —\n• Destroy target artifact or enchantment.\n• Mill five cards, then you may put a land card from your graveyard onto the battlefield tapped.\nFlashback {3}{R}{G}";

/// CR 700.2a: an ordinary modal spell chooses its mode before targets.
#[test]
fn mode_choice_keeps_the_elected_flashback() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    lier(&mut s);
    let relic = s.add_artifact_from_oracle(P1, "Relic", RELIC).id();
    let rg = vec![ManaCostShard::Red, ManaCostShard::Green];
    let wreck = graveyard_card(
        &mut s,
        ("Wreck and Rebuild", false),
        cost(1, rg.clone()),
        &["Flashback"],
        WRECK_AND_REBUILD,
    );
    pool(&mut s, P0, "RGCCC");
    let mut r = s.build();
    let printed = Keyword::Flashback(FlashbackCost::Mana(cost(3, rg)));
    let modes = |w: &WaitingFor| matches!(w, WaitingFor::ModeChoice { .. });
    cast_row(
        &mut r,
        wreck,
        &printed,
        modes,
        aim(&[object(relic)]).modes(&[0]),
    );
    assert_eq!(r.state().objects[&relic].zone, Zone::Graveyard);
    assert_paid(&r, wreck, 1, None);
}

const INSCRIPTION_OF_RUIN: &str = "Kicker {2}{B}{B}\nChoose one. If this spell was kicked, choose any number instead.\n• Target opponent discards two cards.\n• Return target creature card with mana value 2 or less from your graveyard to the battlefield.\n• Destroy target creature with mana value 3 or less.";

/// CR 601.2b + CR 700.2: a kicker that widens the modes is declared before them.
#[test]
fn modal_kicker_keeps_the_elected_flashback() {
    let mut s = GameScenario::new();
    let green = cost(1, vec![ManaCostShard::Green]);
    let bear = s.add_creature(P1, "Bear", 2, 2).with_mana_cost(green).id();
    s.add_card_to_hand(P1, "Hand A");
    s.add_card_to_hand(P1, "Hand B");
    let (mut r, inscription) = doctor_grant(&mut s, |s| {
        let mana = cost(2, vec![ManaCostShard::Black]);
        graveyard_card(
            s,
            ("Inscription of Ruin", false),
            mana,
            &["Kicker"],
            INSCRIPTION_OF_RUIN,
        )
    });
    fund(&mut r, "BBBCCCCRG");
    let kicker = |w: &WaitingFor| {
        matches!(w, WaitingFor::OptionalCostChoice { pending_cast, .. }
            if pending_cast.deferred_modal_choice.is_some())
    };
    let answers = aim(&[player(P1), object(bear)]).paying().modes(&[0, 2]);
    cast_row(&mut r, inscription, &lier_flashback(), kicker, answers);
    assert!(r.state().players[1].hand.is_empty(), "P1 discarded two");
    assert_eq!(r.state().objects[&bear].zone, Zone::Graveyard);
    assert_paid(&r, inscription, 3, Some(ManaType::Black));
}

const ATLANTIS_ATTACKS: &str = "Teamwork 4 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 4 or more.)\nChoose one. If this spell was cast using teamwork, choose both instead.\n• Target player creates a 6/5 blue Leviathan creature token with hexproof.\n• Return one or two target nonland permanents to their owners' hands.";

/// CR 601.2b + CR 700.2: a non-kicker additional cost that widens the modes
/// is declared before the modes.
#[test]
fn conditional_modes_keep_the_elected_flashback() {
    let mut s = GameScenario::new();
    s.add_vanilla(P0, 4, 4);
    let (mut r, atlantis) = doctor_grant(&mut s, |s| {
        let mana = cost(5, vec![ManaCostShard::Blue, ManaCostShard::Blue]);
        graveyard_card(
            s,
            ("Atlantis Attacks", false),
            mana,
            &["Teamwork"],
            ATLANTIS_ATTACKS,
        )
    });
    let object_state = &r.state().objects[&atlantis];
    let widened = object_state.modal.as_ref().is_some_and(|modal| {
        modal.constraints.iter().any(|constraint| {
            matches!(
                constraint,
                ModalSelectionConstraint::ConditionalMaxChoices {
                    condition: ModalSelectionCondition::AdditionalCostPaid {
                        origin: Some(AdditionalCostOrigin::Teamwork),
                        ..
                    },
                    ..
                }
            )
        })
    });
    assert!(widened, "{:?}", object_state.modal);
    let mut kinds = object_state.keywords.iter().map(Keyword::kind);
    assert!(!kinds.any(|kind| kind == KeywordKind::Kicker), "no kicker");
    fund(&mut r, "UUCCCCCRG");
    let modes = |w: &WaitingFor| matches!(w, WaitingFor::ModeChoice { .. });
    let answers = aim(&[player(P0)]).modes(&[0]);
    cast_row(&mut r, atlantis, &lier_flashback(), modes, answers);
    let state = r.state();
    let leviathans = state
        .battlefield
        .iter()
        .filter(|id| state.objects[*id].name.contains("Leviathan"));
    assert_eq!(leviathans.count(), 1);
    assert_paid(&r, atlantis, 3, Some(ManaType::Blue));
}

/// CR 601.2c: an opponent chooses some of the targets while the spell is
/// cast. Volcanic Offering (card-data): "Destroy target nonbasic land you
/// don't control and target nonbasic land of an opponent's choice you don't
/// control. Volcanic Offering deals 7 damage to target creature you don't
/// control and 7 damage to target creature of an opponent's choice you don't
/// control." P1 announces both slots (CR 601.2c: the caster and P1 choose
/// alternately) and picks P2's land and its own other creature.
#[test]
fn announcing_opponent_keeps_the_elected_flashback() {
    let mut s = GameScenario::new_n_player(3, 7);
    let p2 = PlayerId(2);
    let land_one = s
        .add_land_from_oracle(P1, "Ruins One", "{T}: Add {C}.")
        .id();
    let bear = s.add_vanilla(P1, 3, 3);
    let cub = s.add_vanilla(P1, 2, 2);
    let land_two = s
        .add_land_from_oracle(p2, "Ruins Two", "{T}: Add {C}.")
        .id();
    let wolf = s.add_vanilla(p2, 4, 4);
    let db = crate::support::shared_card_db().expect("the integration card database");
    let (mut r, offering) = doctor_grant(&mut s, |s| {
        s.add_real_card(P0, "Volcanic Offering", Zone::Graveyard, db)
    });
    fund(&mut r, "RCCCCG");
    let announce = |w: &WaitingFor| {
        matches!(w, WaitingFor::ChooseAnnouncingOpponent { candidates, .. }
            if candidates.len() == 2 && candidates.contains(&P1) && candidates.contains(&p2))
    };
    elect(&mut r, offering, &lier_flashback());
    for _ in 0..2 {
        let w = &r.state().waiting_for;
        assert!(announce(w), "{w:?}");
        let p1 = GameAction::ChooseAnnouncingOpponent { opponent: P1 };
        r.act(p1).expect("P1 announces");
    }
    for (chooser, pick) in [(P0, land_one), (P1, land_two), (P0, bear), (P1, cub)] {
        let w = &r.state().waiting_for;
        let slot = matches!(w, WaitingFor::TargetSelection { player, .. } if *player == chooser);
        assert!(slot, "{chooser:?} chooses: {w:?}");
        let pending = w.pending_cast_ref().expect("a paused cast");
        assert_eq!(pending.alternative_cost_keyword, Some(lier_flashback()));
        let target = Some(object(pick));
        r.act(GameAction::ChooseTarget { target }).expect("legal");
    }
    finish(&mut r, &lier_flashback(), Answers::default());
    for id in [land_one, land_two, bear, cub] {
        assert_eq!(r.state().objects[&id].zone, Zone::Graveyard);
    }
    assert_eq!(
        r.state().objects[&wolf].zone,
        Zone::Battlefield,
        "not chosen"
    );
    assert_paid(&r, offering, 2, None);
}

/// The public-action companion: once Lier is gone, the printed instance is
/// the only one, so there is no menu and the printed cost is paid.
#[test]
fn printed_flashback_alone_after_lier_dies() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let lier = lier(&mut s);
    let da = deep_analysis(&mut s);
    let murder = s
        .add_spell_to_hand(P0, "Murder", true)
        .with_mana_cost(cost(1, vec![ManaCostShard::Black, ManaCostShard::Black]))
        .from_oracle_text(MURDER)
        .id();
    library(&mut s, 3);
    pool(&mut s, P0, "UUUUBBC");
    let mut r = s.build();
    let paused = elect(&mut r, da, &lier_flashback());
    assert!(
        matches!(paused, WaitingFor::TargetSelection { .. }),
        "{paused:?}"
    );
    r.act(GameAction::CancelCast)
        .expect("the cast is cancelled");
    assert_eq!(r.state().objects[&da].zone, Zone::Graveyard);
    r.cast(murder).target_object(lier).resolve();
    assert_eq!(r.state().objects[&lier].zone, Zone::Graveyard);
    assert_eq!(instances(&r, da, KeywordKind::Flashback).len(), 1);
    assert!(menu(&r, da).is_empty(), "one instance: no menu");
    r.cast(da).target_player(P0).resolve();
    assert_eq!(r.life(P0), 17, "the printed instance pays 3 life");
    assert_eq!(r.state().objects[&da].zone, Zone::Exile);
    assert_settled(&r);
}

fn strip_carrier(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("alternative_cost_keyword");
            map.values_mut().for_each(strip_carrier);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_carrier),
        _ => {}
    }
}

fn restore(r: &GameRunner, legacy: bool) -> GameRunner {
    let mut value = serde_json::to_value(r.state()).expect("the paused state serializes");
    if legacy {
        strip_carrier(&mut value);
    } else {
        let carried = value.to_string().contains("alternative_cost_keyword");
        assert!(carried, "reach guard");
    }
    GameRunner::from_state(serde_json::from_value(value).expect("the paused state restores"))
}

/// A paused keyed cast survives serialization and pays its instance.
#[test]
fn paused_keyed_cast_round_trips() {
    let (mut r, da) = lier_board(20, 4);
    let printed = instances(&r, da, KeywordKind::Flashback)[0].clone();
    let paused = elect(&mut r, da, &printed);
    assert!(
        matches!(paused, WaitingFor::TargetSelection { .. }),
        "{paused:?}"
    );
    let mut r = restore(&r, false);
    finish(&mut r, &printed, aim(&[player(P0)]));
    assert_eq!(r.life(P0), 17);
    assert_paid(&r, da, 3, None);
}

/// A legacy pending cast (no carrier) over two instances can't pick one: it
/// is refused with nothing paid, and the cast can be cancelled.
#[test]
fn legacy_ambiguous_pending_cast_is_refused() {
    let (mut r, da) = lier_board(20, 4);
    let printed = instances(&r, da, KeywordKind::Flashback)[0].clone();
    elect(&mut r, da, &printed);
    let mut r = restore(&r, true);
    let target = Some(player(P0));
    let refused = r.act(GameAction::ChooseTarget { target });
    let ambiguous = |error: &_| format!("{error:?}").contains("announce which one is paid");
    assert!(
        refused.as_ref().is_err_and(ambiguous),
        "{:?}",
        refused.err()
    );
    assert_eq!(r.life(P0), 20);
    assert_eq!(pool_colors(&r).len(), 5, "nothing paid");
    r.act(GameAction::CancelCast)
        .expect("the cast is cancelled");
    assert_eq!(r.state().objects[&da].zone, Zone::Graveyard);
}

/// A legacy pending cast over a single instance resumes as before.
#[test]
fn legacy_single_instance_pending_cast_completes() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let da = deep_analysis(&mut s);
    library(&mut s, 3);
    pool(&mut s, P0, "UU");
    let mut r = s.build();
    start_cast(&mut r, da);
    let waiting = &r.state().waiting_for;
    assert!(
        matches!(waiting, WaitingFor::TargetSelection { .. }),
        "{waiting:?}"
    );
    let mut r = restore(&r, true);
    let target = Some(player(P0));
    r.act(GameAction::ChooseTarget { target })
        .expect("the target is chosen");
    r.advance_until_stack_empty();
    assert_eq!(r.life(P0), 17);
    assert_eq!(r.state().objects[&da].zone, Zone::Exile);
    assert_settled(&r);
}

/// CR 608.2g + CR 118.9a: a free cast an effect offers during resolution
/// (Invoke Calamity) is bound to that offer's permission, its one alternative
/// cost: it pays neither flashback cost, with Lier or without.
#[test]
fn a_resolution_free_cast_pays_no_flashback_cost() {
    let db = crate::support::shared_card_db().expect("the integration card database");
    for with_lier in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        if with_lier {
            lier(&mut s);
        }
        let invoke = s.add_real_card(P0, "Invoke Calamity", Zone::Hand, db);
        let da = deep_analysis(&mut s);
        library(&mut s, 3);
        pool(&mut s, P0, "RRRRC");
        let mut r = s.build();
        start_cast(&mut r, invoke);
        r.act(GameAction::PassPriority).expect("P0 passes");
        r.act(GameAction::PassPriority).expect("P1 passes");
        let w = &r.state().waiting_for;
        let offered = matches!(w, WaitingFor::CastOffer { .. } if format!("{w:?}").contains(&format!("candidates: [{da:?}]")));
        assert!(
            offered,
            "Deep Analysis is offered (Lier: {with_lier}): {w:?}"
        );
        let selection = Some(da);
        r.act(GameAction::FreeCastWindowChoice { selection })
            .expect("free cast");
        let target = Some(player(P0));
        r.act(GameAction::ChooseTarget { target }).expect("target");
        r.advance_until_stack_empty();
        assert_eq!(r.life(P0), 20, "no flashback life (Lier: {with_lier})");
        assert_eq!(r.state().players[0].hand.len(), 2, "drew two");
        assert_paid(&r, da, 1, None);
        assert_settled(&r);
    }
}

/// CR 601.2g-h: a sole keyed option (Lier's {1}{U} Think Twice; the printed
/// {2}{U} isn't affordable) payable only with Blood Pet keeps the explicit
/// choice of that sacrificial source.
#[test]
fn a_sole_keyed_option_keeps_the_sacrificial_mana_choice() {
    let db = crate::support::shared_card_db().expect("the integration card database");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    lier(&mut s);
    let think = s.add_real_card(P0, "Think Twice", Zone::Graveyard, db);
    s.add_real_card(P0, "Island", Zone::Battlefield, db);
    let pet = s.add_real_card(P0, "Blood Pet", Zone::Battlefield, db);
    library(&mut s, 2);
    let mut r = s.build();
    let cast = legal_actions(r.state()).into_iter().find(|action| {
        matches!(action, GameAction::CastSpell { object_id, payment_mode: CastPaymentMode::AutoExceptSacrificialMana, .. } if *object_id == think)
    });
    let cast = cast.expect("the cast keeps the sacrificial-source choice");
    r.act(cast).expect("the cast is accepted");
    let WaitingFor::ManaSourceSelection { options, .. } = &r.state().waiting_for else {
        panic!("a mana-source choice, got {:?}", r.state().waiting_for);
    };
    assert_eq!(
        r.state().objects[&pet].zone,
        Zone::Battlefield,
        "not yet sacrificed"
    );
    let selection = options.iter().find(|o| o.source.object_id == pet).cloned();
    let selection = selection.expect("Blood Pet is offered");
    r.act(GameAction::ActivateManaSource { selection })
        .expect("Blood Pet pays");
    finish(&mut r, &lier_flashback(), Answers::default());
    assert_eq!(r.state().objects[&pet].zone, Zone::Graveyard);
    assert_eq!(r.state().objects[&think].zone, Zone::Exile);
    assert_eq!(r.state().players[0].hand.len(), 1, "drew one");
}

/// CR 702.62a + CR 608.2g: the suspend last-counter cast is a resolution cast;
/// binding its permission keeps the suspend cast, so it has haste.
#[test]
fn a_suspend_cast_keeps_its_haste() {
    let db = crate::support::shared_card_db().expect("the integration card database");
    let mut s = GameScenario::new();
    s.at_phase(Phase::Untap);
    let baloth = s.add_real_card(P0, "Durkwood Baloth", Zone::Exile, db);
    s.with_counter(baloth, CounterType::Time, 1);
    library(&mut s, 2);
    let mut r = s.build();
    for _ in 0..8 {
        if matches!(
            r.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    r.act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the last counter offers the cast");
    r.advance_until_stack_empty();
    assert_eq!(r.state().objects[&baloth].zone, Zone::Battlefield);
    r.advance_to_combat();
    r.declare_attackers(&[(baloth, AttackTarget::Player(P1))])
        .expect("haste: it attacks the turn it entered");
}

/// CR 608.2g + CR 601.2a: a free cast bound to Invoke Calamity's permission
/// elects no other graveyard permission, so Kess, Dissident Mage's
/// once-per-turn slot stays unused for a later cast.
#[test]
fn a_resolution_free_cast_leaves_kess_unused() {
    let db = crate::support::shared_card_db().expect("the integration card database");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_real_card(P0, "Kess, Dissident Mage", Zone::Battlefield, db);
    let invoke = s.add_real_card(P0, "Invoke Calamity", Zone::Hand, db);
    let da = deep_analysis(&mut s);
    let divination = s.add_real_card(P0, "Divination", Zone::Graveyard, db);
    library(&mut s, 6);
    pool(&mut s, P0, "RRRRC");
    let mut r = s.build();
    start_cast(&mut r, invoke);
    r.act(GameAction::PassPriority).expect("P0 passes");
    r.act(GameAction::PassPriority).expect("P1 passes");
    let selection = Some(da);
    r.act(GameAction::FreeCastWindowChoice { selection })
        .expect("free cast");
    let target = Some(player(P0));
    r.act(GameAction::ChooseTarget { target }).expect("target");
    r.advance_until_stack_empty();
    assert_eq!(r.life(P0), 20, "no flashback life");
    assert_eq!(r.state().players[0].hand.len(), 2, "drew two");
    assert_settled(&r);
    fund(&mut r, "UUU");
    assert_eq!(casts_offered(&r, divination), 1, "Kess is unused");
    r.cast(divination).resolve();
    assert_eq!(r.state().players[0].hand.len(), 4, "drew two more");
    assert_eq!(r.state().objects[&divination].zone, Zone::Exile, "Kess");
    assert_settled(&r);
}

/// CR 601.2g-h: the menu answer classifies payment from the elected option.
/// Printed {2}{U} needs Blood Pet, so it is asked for and kept until chosen;
/// Lier's {1}{U} is paid by the two Islands and Blood Pet stays.
#[test]
fn a_menu_answer_keeps_the_sacrificial_mana_choice() {
    let db = crate::support::shared_card_db().expect("the integration card database");
    let printed = Keyword::Flashback(FlashbackCost::Mana(cost(2, vec![ManaCostShard::Blue])));
    for keyword in [printed, lier_flashback()] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        lier(&mut s);
        let think = s.add_real_card(P0, "Think Twice", Zone::Graveyard, db);
        s.add_real_card(P0, "Island", Zone::Battlefield, db);
        s.add_real_card(P0, "Island", Zone::Battlefield, db);
        let pet = s.add_real_card(P0, "Blood Pet", Zone::Battlefield, db);
        library(&mut s, 2);
        let mut r = s.build();
        let cast = legal_actions(r.state()).into_iter().find(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == think),
        );
        r.act(cast.expect("Think Twice is offered"))
            .expect("the cast is accepted");
        let WaitingFor::CastingVariantChoice { options, .. } = r.state().waiting_for.clone() else {
            panic!("two options, got {:?}", r.state().waiting_for);
        };
        assert_eq!(options.len(), 2, "{options:?}");
        let index = options
            .iter()
            .position(|o| o.alternative_cost_keyword.as_ref() == Some(&keyword));
        let index = index.expect("the instance is offered");
        r.act(GameAction::ChooseCastingVariant { index })
            .expect("the option is chosen");
        let needs_pet = keyword != lier_flashback();
        if needs_pet {
            let WaitingFor::ManaSourceSelection { options, .. } = &r.state().waiting_for else {
                panic!("a mana-source choice, got {:?}", r.state().waiting_for);
            };
            assert_eq!(r.state().objects[&pet].zone, Zone::Battlefield);
            let selection = options.iter().find(|o| o.source.object_id == pet).cloned();
            let selection = selection.expect("Blood Pet is offered");
            r.act(GameAction::ActivateManaSource { selection })
                .expect("Blood Pet pays");
        }
        finish(&mut r, &keyword, Answers::default());
        let pet_zone = if needs_pet {
            Zone::Graveyard
        } else {
            Zone::Battlefield
        };
        assert_eq!(r.state().objects[&pet].zone, pet_zone);
        assert_eq!(r.state().objects[&think].zone, Zone::Exile);
        assert_eq!(r.state().players[0].hand.len(), 1, "drew one");
    }
}
