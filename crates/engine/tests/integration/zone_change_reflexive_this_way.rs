//! CR 603.12: the "When <zone change> this way, <body>" reflexive triggered
//! ability class (#9762). The head names a zone change an earlier instruction
//! of the same resolution performed; the reflexive is created after it, its
//! targets are chosen as it is put on the stack (CR 603.3d), and its body reads
//! the moved objects through referents the stack object carries.
//!
//! Shapes the engine cannot carry fail closed at parse time (the gate's event
//! must come from the instruction just before it; the body's object reference
//! must not be an unbound parent target).

use std::collections::VecDeque;

use engine::game::combat::{has_cant_be_blocked_static, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, PersistedGameState, PersistedRestoreFinalization, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// What happened while driving one resolution to an empty stack.
#[derive(Default)]
struct Drive {
    /// Zone of the watched object at each target prompt, in order.
    target_prompts: Vec<Zone>,
    /// Cards offered at each `EffectZoneChoice`, in order.
    zone_choices: Vec<Vec<ObjectId>>,
    /// Distinct triggered abilities seen on top of the stack at a priority
    /// window.
    triggers_on_stack: usize,
    seen_triggers: Vec<ObjectId>,
    /// `DiscardChoice` prompts answered.
    discard_choices: usize,
    /// `OptionalEffectChoice` prompts answered.
    optional_prompts: usize,
    /// `ReplacementChoice` prompts answered.
    replacement_prompts: usize,
}

struct Plan {
    watched: ObjectId,
    targets: VecDeque<Vec<TargetRef>>,
    picks: Vec<ObjectId>,
    accept_optional: bool,
    apply_replacement: bool,
}

impl Plan {
    fn new(watched: ObjectId) -> Self {
        Self {
            watched,
            targets: VecDeque::new(),
            picks: Vec::new(),
            accept_optional: true,
            apply_replacement: true,
        }
    }
    fn decline_replacement(mut self) -> Self {
        self.apply_replacement = false;
        self
    }
    fn target(mut self, targets: Vec<TargetRef>) -> Self {
        self.targets.push_back(targets);
        self
    }
    fn picks(mut self, picks: Vec<ObjectId>) -> Self {
        self.picks = picks;
        self
    }
    fn decline_optional(mut self) -> Self {
        self.accept_optional = false;
        self
    }
}

fn pick(plan: &Plan, offered: &[ObjectId], count: usize) -> Vec<ObjectId> {
    plan.picks
        .iter()
        .copied()
        .filter(|card| offered.contains(card))
        .take(count)
        .collect()
}

fn drive(r: &mut GameRunner, mut plan: Plan) -> Drive {
    let mut out = Drive::default();
    for _ in 0..80 {
        if std::env::var_os("ZCR_TRACE").is_some() {
            let text = format!("{:?}", r.state().waiting_for);
            eprintln!("TRACE {}", &text[..text.len().min(400)]);
        }
        let result =
            match r.state().waiting_for.clone() {
                WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                    out.target_prompts
                        .push(r.state().objects[&plan.watched].zone);
                    let targets = plan.targets.pop_front().unwrap_or_default();
                    r.act(GameAction::SelectTargets { targets })
                }
                WaitingFor::DiscardChoice { cards, count, .. } => {
                    out.discard_choices += 1;
                    let chosen = pick(&plan, &cards, count);
                    r.act(GameAction::SelectCards { cards: chosen })
                }
                WaitingFor::EffectZoneChoice { cards, count, .. } => {
                    out.zone_choices.push(cards.clone());
                    let chosen = pick(&plan, &cards, count);
                    r.act(GameAction::SelectCards { cards: chosen })
                }
                WaitingFor::DigChoice {
                    cards, keep_count, ..
                } => {
                    let chosen = pick(&plan, &cards, keep_count);
                    r.act(GameAction::SelectCards { cards: chosen })
                }
                WaitingFor::ScryChoice { .. } => r.act(GameAction::SelectCards { cards: vec![] }),
                WaitingFor::OptionalEffectChoice { .. } => {
                    out.optional_prompts += 1;
                    r.act(GameAction::DecideOptionalEffect {
                        accept: plan.accept_optional,
                    })
                }
                WaitingFor::ReplacementChoice { candidates, .. } => {
                    out.replacement_prompts += 1;
                    let index = candidates
                        .iter()
                        .position(|candidate| {
                            (candidate.description == "Decline") != plan.apply_replacement
                        })
                        .unwrap_or(0);
                    r.act(GameAction::ChooseReplacement { index })
                }
                WaitingFor::ManaPayment { .. } => r.act(GameAction::PassPriority),
                WaitingFor::Priority { .. } if r.state().stack.is_empty() => break,
                WaitingFor::Priority { .. } => {
                    if let Some(entry) = r.state().stack.back().filter(|entry| {
                        matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                    }) {
                        if !out.seen_triggers.contains(&entry.id) {
                            out.seen_triggers.push(entry.id);
                            out.triggers_on_stack += 1;
                        }
                    }
                    r.act(GameAction::PassPriority)
                }
                other => panic!("unexpected prompt {other:?}"),
            };
        result.expect("action accepted");
    }
    out
}

fn cast(r: &mut GameRunner, spell: ObjectId) {
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");
}

fn counters(r: &GameRunner, id: ObjectId) -> u32 {
    r.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn spell_ability(text: &str, name: &str) -> AbilityDefinition {
    let parsed = parse_oracle_text(text, name, &[], &["Sorcery".to_string()], &[]);
    parsed
        .abilities
        .into_iter()
        .next()
        .expect("one spell ability")
}

/// The first `Unimplemented` name along the ability's sub chain.
fn unimplemented_name(def: &AbilityDefinition) -> Option<String> {
    let mut node = Some(def);
    while let Some(current) = node {
        if let Effect::Unimplemented { name, .. } = current.effect.as_ref() {
            return Some(name.clone());
        }
        node = current.sub_ability.as_deref();
    }
    None
}

/// Reach guard for a negative: the printed chain is the supported reflexive
/// shape (no `Unimplemented`, and a node gated by the `WhenYouDo` creation
/// marker with a zone-change event), so "nothing happened" can't come from a
/// fail-closed parse.
fn assert_supported_zone_change_reflexive(def: &AbilityDefinition) {
    assert_eq!(unimplemented_name(def), None, "supported chain: {def:?}");
    let mut node = Some(def);
    while let Some(current) = node {
        if current.condition.as_ref().is_some_and(|condition| {
            condition.has_when_you_do_marker() && condition.zone_changed_this_way_head().is_some()
        }) {
            return;
        }
        node = current.sub_ability.as_deref();
    }
    panic!("no zone-change reflexive gate in {def:?}");
}

// ---------------------------------------------------------------------------
// Discard heads
// ---------------------------------------------------------------------------

const GLACIAL_DRAGONHUNT: &str = "Draw a card, then you may discard a card. When you discard a nonland card this way, Glacial Dragonhunt deals 3 damage to target creature.";

fn glacial_fixture() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Glacial Dragonhunt", false, GLACIAL_DRAGONHUNT)
        .id();
    let three = s
        .add_spell_to_hand(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    s.add_card_to_library_top(P0, "Library Card");
    let opp = s.add_creature(P1, "Opp Giant", 4, 4).id();
    s.add_creature(P1, "Opp Cub", 1, 1);
    (s.build(), spell, three, opp)
}

/// CR 603.12 + CR 603.3d: Glacial Dragonhunt announces no target as it is cast;
/// the reflexive asks for "target creature" after the nonland discard.
#[test]
fn glacial_dragonhunt_targets_after_the_discard() {
    let (mut r, spell, three, opp) = glacial_fixture();
    cast(&mut r, spell);
    let out = drive(
        &mut r,
        Plan::new(three)
            .target(vec![TargetRef::Object(opp)])
            .picks(vec![three]),
    );
    assert_eq!(
        out.target_prompts,
        vec![Zone::Graveyard],
        "one target prompt, after the card was discarded"
    );
    assert_eq!(r.state().objects[&opp].damage_marked, 3);
}

/// CR 603.12: declining the optional discard means the event never happened.
#[test]
fn glacial_dragonhunt_decline_creates_no_reflexive() {
    assert_supported_zone_change_reflexive(&spell_ability(
        GLACIAL_DRAGONHUNT,
        "Glacial Dragonhunt",
    ));
    let (mut r, spell, three, opp) = glacial_fixture();
    cast(&mut r, spell);
    let out = drive(&mut r, Plan::new(three).decline_optional());
    assert_eq!(
        out.optional_prompts, 1,
        "reach: the optional discard was offered, then declined"
    );
    assert_eq!(
        out.discard_choices, 0,
        "declined before any card was chosen"
    );
    assert!(out.target_prompts.is_empty(), "no target prompt");
    assert_eq!(out.triggers_on_stack, 0, "no reflexive on the stack");
    assert_eq!(
        r.state().objects[&three].zone,
        Zone::Hand,
        "reach: nothing discarded"
    );
    assert_eq!(r.state().objects[&opp].damage_marked, 0);
}

/// Borborygmos and Fblthp's body: "twice that much" reads the number discarded
/// this way after the reflexive leaves the creating resolution.
#[test]
fn borborygmos_body_twice_that_much_survives_the_stack() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe",
            false,
            "Draw a card, then you may discard any number of land cards. When you discard one or more cards this way, Probe deals twice that much damage to target creature.",
        )
        .id();
    let l1 = s.add_land_to_hand(P0, "Land One").id();
    let l2 = s.add_land_to_hand(P0, "Land Two").id();
    s.add_card_to_library_top(P0, "Library Card");
    let opp = s.add_creature(P1, "Opp Giant", 9, 9).id();
    s.add_creature(P1, "Opp Cub", 1, 1);
    let mut r = s.build();
    cast(&mut r, spell);
    let out = drive(
        &mut r,
        Plan::new(l1)
            .target(vec![TargetRef::Object(opp)])
            .picks(vec![l1, l2]),
    );
    assert_eq!(out.target_prompts, vec![Zone::Graveyard]);
    assert_eq!(
        r.state().objects[&opp].damage_marked,
        4,
        "twice the two discarded"
    );
}

// ---------------------------------------------------------------------------
// Redirected discards (CR 701.9a + CR 702.35a): a discarded card a
// replacement sends elsewhere is still discarded "this way".
// ---------------------------------------------------------------------------

const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

const LIBRARY_OF_LENG: &str = "You have no maximum hand size.\nIf an effect causes you to discard a card, discard it, but you may put it on top of your library instead of into your graveyard.";

/// Probe P1: Glacial Dragonhunt's chosen discard is exiled by Rest in Peace on
/// its way out of the hand; the reflexive still triggers and deals 3.
#[test]
fn glacial_dragonhunt_reflexive_fires_when_rest_in_peace_exiles_the_discard() {
    let (mut r, spell, three, opp) = {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let spell = s
            .add_spell_to_hand_from_oracle(P0, "Glacial Dragonhunt", false, GLACIAL_DRAGONHUNT)
            .id();
        let three = s
            .add_spell_to_hand(P0, "Three Drop", false)
            .with_mana_cost(ManaCost::generic(3))
            .id();
        s.add_card_to_library_top(P0, "Library Card");
        let opp = s.add_creature(P1, "Opp Giant", 4, 4).id();
        s.add_creature(P1, "Opp Cub", 1, 1);
        s.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
        (s.build(), spell, three, opp)
    };
    cast(&mut r, spell);
    let out = drive(
        &mut r,
        Plan::new(three)
            .target(vec![TargetRef::Object(opp)])
            .picks(vec![three]),
    );
    assert_eq!(out.discard_choices, 1, "reach: an actual discard choice");
    assert_eq!(
        r.state().objects[&three].zone,
        Zone::Exile,
        "reach: Rest in Peace sent the discarded card to exile"
    );
    assert_eq!(
        out.target_prompts,
        vec![Zone::Exile],
        "one target prompt, after the card was discarded into exile"
    );
    assert_eq!(out.triggers_on_stack, 1, "one reflexive on the stack");
    assert_eq!(r.state().objects[&opp].damage_marked, 3);
}

/// Control: a forced discard with no choice (the hand's only card) under Rest
/// in Peace publishes through the synchronous chain path, unchanged.
#[test]
fn forced_discard_under_rest_in_peace_fires_the_reflexive() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe",
            false,
            "Discard a card. When you discard a nonland card this way, Probe deals 3 damage to target creature.",
        )
        .id();
    let three = s
        .add_spell_to_hand(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let opp = s.add_creature(P1, "Opp Giant", 4, 4).id();
    s.add_creature(P1, "Opp Cub", 1, 1);
    s.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let mut r = s.build();
    cast(&mut r, spell);
    let out = drive(
        &mut r,
        Plan::new(three).target(vec![TargetRef::Object(opp)]),
    );
    assert_eq!(out.discard_choices, 0, "reach: no choice, a forced discard");
    assert_eq!(r.state().objects[&three].zone, Zone::Exile);
    assert_eq!(out.target_prompts, vec![Zone::Exile]);
    assert_eq!(r.state().objects[&opp].damage_marked, 3);
}

/// The replacement-paused path: Library of Leng asks whether the chosen discard
/// goes on top of the library instead (modelled as hand → graveyard →
/// library). Applied or declined, the card is discarded and the reflexive
/// triggers. No instruction precedes the discard, so only this discard can
/// satisfy the gate.
fn library_of_leng_discard(apply: bool) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe",
            false,
            "Discard a card. When you discard a card this way, Probe deals 3 damage to target creature.",
        )
        .id();
    let three = s
        .add_spell_to_hand(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    s.add_spell_to_hand(P0, "Two Drop", false)
        .with_mana_cost(ManaCost::generic(2));
    let opp = s.add_creature(P1, "Opp Giant", 4, 4).id();
    s.add_creature(P1, "Opp Cub", 1, 1);
    s.add_enchantment_from_oracle(P0, "Library of Leng", LIBRARY_OF_LENG);
    let mut r = s.build();
    cast(&mut r, spell);
    let plan = Plan::new(three)
        .target(vec![TargetRef::Object(opp)])
        .picks(vec![three]);
    let plan = if apply {
        plan
    } else {
        plan.decline_replacement()
    };
    let out = drive(&mut r, plan);
    assert_eq!(out.discard_choices, 1, "reach: an actual discard choice");
    assert_eq!(
        out.replacement_prompts, 1,
        "reach: the discard paused on Library of Leng's choice"
    );
    let landed = if apply {
        Zone::Library
    } else {
        Zone::Graveyard
    };
    assert_eq!(r.state().objects[&three].zone, landed);
    assert_eq!(out.target_prompts, vec![landed]);
    assert_eq!(r.state().objects[&opp].damage_marked, 3);
}

#[test]
fn library_of_leng_redirected_discard_still_triggers_the_reflexive() {
    library_of_leng_discard(true);
}

#[test]
fn library_of_leng_declined_discard_to_the_graveyard_is_unchanged() {
    library_of_leng_discard(false);
}

const VOHAR: &str = "{T}: Draw a card, then discard a card. If you discarded an instant or sorcery card this way, each opponent loses 1 life and you gain 1 life.\n{2}, Sacrifice Vohar: You may cast target instant or sorcery card from your graveyard this turn. If that spell would be put into your graveyard, exile it instead. Activate only as a sorcery.";

/// Vohar, Vodalian Desecrator: the "If" sibling over the same discard
/// completion. With Rest in Peace the discarded instant is exiled, and is still
/// "discarded this way".
fn vohar_discards_an_instant(rest_in_peace: bool) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let vohar = s
        .add_creature_from_oracle(P0, "Vohar, Vodalian Desecrator", 1, 2, VOHAR)
        .id();
    let bolt = s.add_spell_to_hand(P0, "Bolt", true).id();
    s.add_creature_to_hand(P0, "Hand Bear", 2, 2);
    s.add_card_to_library_top(P0, "Library Card");
    if rest_in_peace {
        s.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    }
    let mut r = s.build();
    let (p0, p1) = (r.life(P0), r.life(P1));
    r.act(GameAction::ActivateAbility {
        source_id: vohar,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("activate");
    let out = drive(&mut r, Plan::new(bolt).picks(vec![bolt]));
    assert_eq!(out.discard_choices, 1, "reach: an actual discard choice");
    let landed = if rest_in_peace {
        Zone::Exile
    } else {
        Zone::Graveyard
    };
    assert_eq!(r.state().objects[&bolt].zone, landed);
    assert_eq!(r.life(P1), p1 - 1, "each opponent loses 1 life");
    assert_eq!(r.life(P0), p0 + 1, "you gain 1 life");
}

#[test]
fn vohar_discarded_instant_drains() {
    vohar_discards_an_instant(false);
}

#[test]
fn vohar_discarded_instant_exiled_by_rest_in_peace_still_drains() {
    vohar_discards_an_instant(true);
}

// ---------------------------------------------------------------------------
// Printed discard-head reflexives through their production entry points
// ---------------------------------------------------------------------------

const EVIE_FRYE: &str = "Partner with Jacob Frye (When this creature enters, target player may put Jacob into their hand from their library, then shuffle.)\n{1}, {T}: Draw a card, then discard a card. When you discard a creature card this way, target creature you control can't be blocked this turn.";

/// Evie Frye's activated ability: the reflexive's target is chosen after the
/// creature card is discarded, and that creature can't be blocked.
#[test]
fn evie_frye_activation_reflexive_targets_after_the_discard() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let evie = s
        .add_creature_from_oracle(P0, "Evie Frye", 2, 1, EVIE_FRYE)
        .id();
    s.add_basic_land(P0, ManaColor::Blue);
    let ally = s.add_creature(P0, "Ally", 2, 2).id();
    let hand_bear = s.add_creature_to_hand(P0, "Hand Bear", 2, 2).id();
    s.add_card_to_library_top(P0, "Library Card");
    let mut r = s.build();
    r.act(GameAction::ActivateAbility {
        source_id: evie,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("activate");
    let out = drive(
        &mut r,
        Plan::new(hand_bear)
            .target(vec![TargetRef::Object(ally)])
            .picks(vec![hand_bear]),
    );
    assert_eq!(out.discard_choices, 1, "reach: an actual discard choice");
    assert_eq!(
        out.target_prompts,
        vec![Zone::Graveyard],
        "one target prompt, after the creature card was discarded"
    );
    assert!(has_cant_be_blocked_static(r.state(), ally));
    assert!(!has_cant_be_blocked_static(r.state(), evie));
}

const SERUM_CORE_CHIMERA: &str = "Flying\nWhenever you cast a noncreature spell, put an oil counter on this creature.\nRemove three oil counters from this creature: Draw a card. Then you may discard a nonland card. When you discard a card this way, this creature deals 3 damage to target creature or planeswalker. Activate only as a sorcery.";

/// Serum-Core Chimera's oil-counter activation: the damage's target is chosen
/// after the optional nonland discard.
#[test]
fn serum_core_chimera_activation_reflexive_targets_after_the_discard() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let chimera = s
        .add_creature_from_oracle(P0, "Serum-Core Chimera", 2, 4, SERUM_CORE_CHIMERA)
        .id();
    s.with_counter(chimera, CounterType::Generic("oil".to_string()), 3);
    let three = s
        .add_spell_to_hand(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    s.add_card_to_library_top(P0, "Library Card");
    let opp = s.add_creature(P1, "Opp Giant", 4, 4).id();
    s.add_creature(P1, "Opp Cub", 1, 1);
    let mut r = s.build();
    r.act(GameAction::ActivateAbility {
        source_id: chimera,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("activate");
    assert_eq!(
        r.state().objects[&chimera]
            .counters
            .get(&CounterType::Generic("oil".to_string()))
            .copied()
            .unwrap_or(0),
        0,
        "reach: the three oil counters were removed as the cost"
    );
    let out = drive(
        &mut r,
        Plan::new(three)
            .target(vec![TargetRef::Object(opp)])
            .picks(vec![three]),
    );
    assert_eq!(out.optional_prompts, 1, "reach: the optional discard");
    assert_eq!(out.discard_choices, 1, "reach: an actual discard choice");
    assert_eq!(out.target_prompts, vec![Zone::Graveyard]);
    assert_eq!(r.state().objects[&opp].damage_marked, 3);
}

const TEO: &str = "Flying\nWhenever one or more creatures you control with flying attack, draw a card, then discard a card. When you discard a nonland card this way, put a +1/+1 counter on target creature you control.";

/// Teo, Spirited Glider's attack trigger: the counter's target is chosen after
/// the nonland discard.
#[test]
fn teo_attack_trigger_reflexive_targets_after_the_discard() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let teo = s
        .add_creature_from_oracle(P0, "Teo, Spirited Glider", 1, 4, TEO)
        .id();
    let ally = s.add_creature(P0, "Ally", 2, 2).id();
    let three = s
        .add_spell_to_hand(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    s.add_card_to_library_top(P0, "Library Card");
    let mut r = s.build();
    r.advance_to_phase(Phase::DeclareAttackers);
    r.declare_attackers(&[(teo, AttackTarget::Player(P1))])
        .expect("Teo attacks");
    let out = drive(
        &mut r,
        Plan::new(three)
            .target(vec![TargetRef::Object(ally)])
            .picks(vec![three]),
    );
    assert_eq!(out.discard_choices, 1, "reach: an actual discard choice");
    assert_eq!(out.target_prompts, vec![Zone::Graveyard]);
    assert_eq!(counters(&r, ally), 1);
    assert_eq!(counters(&r, teo), 0);
}

// ---------------------------------------------------------------------------
// Exile heads
// ---------------------------------------------------------------------------

const HIDETSUGU_BODY: &str = "Exile the top card of your library. You may play that card this turn. When you exile a nonland card this way, Probe deals damage equal to the exiled card's mana value to any target.";

/// Hidetsugu, Devouring Chaos's ability body: the play permission between the
/// exile and the head is not the exiling instruction (CR 608.2c), so the
/// reflexive still sees the exiled card, targets after the exile, and deals
/// that card's mana value.
#[test]
fn hidetsugu_body_reflexive_reads_the_exile_across_the_play_permission() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Probe", false, HIDETSUGU_BODY)
        .id();
    let top = s
        .add_spell_to_library_top(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut r = s.build();
    cast(&mut r, spell);
    let out = drive(&mut r, Plan::new(top).target(vec![TargetRef::Player(P1)]));
    assert_eq!(out.target_prompts, vec![Zone::Exile]);
    assert_eq!(r.life(P1), 17);
    assert!(
        !r.state().objects[&top].casting_permissions.is_empty(),
        "the play permission still stands"
    );
}

/// The "If" connector over the same ledger: a resolution-time condition after
/// the play permission (announced with the spell, CR 601.2c) now resolves.
#[test]
fn if_exiled_this_way_after_a_play_permission_resolves() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe",
            false,
            "Exile the top card of your library. You may play that card this turn. If you exiled a nonland card this way, Probe deals 2 damage to target player.",
        )
        .id();
    let top = s
        .add_spell_to_library_top(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut r = s.build();
    cast(&mut r, spell);
    let out = drive(&mut r, Plan::new(top).target(vec![TargetRef::Player(P1)]));
    assert_eq!(
        out.target_prompts,
        vec![Zone::Library],
        "announced with the spell"
    );
    assert_eq!(r.state().objects[&top].zone, Zone::Exile);
    assert_eq!(r.life(P1), 18);
}

const AGATHA_BODY: &str = "Exile target card from a graveyard. When a creature card is exiled this way, put a +1/+1 counter on target creature you control.";

/// Agatha's Soul Cauldron's ability body: the parent announces its own target;
/// the reflexive announces a second one after the exile.
#[test]
fn agatha_body_reflexive_targets_after_a_creature_card_is_exiled() {
    for exile_a_creature in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let spell = s
            .add_spell_to_hand_from_oracle(P0, "Probe", false, AGATHA_BODY)
            .id();
        let dead = if exile_a_creature {
            s.add_creature_to_graveyard(P1, "Dead Bear", 2, 2).id()
        } else {
            s.add_spell_to_graveyard(P1, "Old Sorcery", false).id()
        };
        // A second graveyard card, so the spell's own target is a real choice.
        s.add_land_to_graveyard(P1, "Old Land");
        let bear = s.add_creature(P0, "Bear", 2, 2).id();
        s.add_creature(P0, "Cub", 1, 1);
        let mut r = s.build();
        cast(&mut r, spell);
        let out = drive(
            &mut r,
            Plan::new(dead)
                .target(vec![TargetRef::Object(dead)])
                .target(vec![TargetRef::Object(bear)]),
        );
        assert_eq!(r.state().objects[&dead].zone, Zone::Exile, "reach: exiled");
        if exile_a_creature {
            assert_eq!(out.target_prompts, vec![Zone::Graveyard, Zone::Exile]);
            assert_eq!(counters(&r, bear), 1);
        } else {
            // CR 603.12: a noncreature card exiled is not the trigger event.
            assert_eq!(out.target_prompts, vec![Zone::Graveyard]);
            assert_eq!(counters(&r, bear), 0);
        }
    }
}

/// Augusta, Order Returned (fail closed): "each player exiles a card from their
/// graveyard" resolves its gated clause once per player, so the single CR
/// 603.12 event "one or more nonland cards are exiled this way" would create
/// one reflexive per player. Measured on the spell form: the first reflexive's
/// target prompt opened while the opponent's card was still in the graveyard.
#[test]
fn augusta_reflexive_over_a_player_iteration_fails_closed() {
    let parsed = parse_oracle_text(
        "Whenever Augusta attacks, each player exiles a card from their graveyard. When one or more nonland cards are exiled this way, put that many +1/+1 counters on target attacking creature.",
        "Augusta, Order Returned",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let execute = parsed.triggers[0].execute.as_deref().expect("trigger body");
    assert!(
        execute.player_scope.is_some(),
        "reach guard: the producer iterates players"
    );
    assert_eq!(
        unimplemented_name(execute).as_deref(),
        Some("zone_change_reflexive_event_source"),
        "{execute:?}"
    );
}

// ---------------------------------------------------------------------------
// Sacrifice heads ("that many")
// ---------------------------------------------------------------------------

/// Probe board 3, Ravenous Rotbelly (fail closed): its reflexive body iterates
/// over players ("each opponent sacrifices that many"), and a player-scoped
/// reflexive body resolves inside the creating resolution instead of waiting
/// on the stack as its own object, so the printed card is not claimed. (Its
/// "up to three Zombies" head also offers only one; listed separately.)
#[test]
fn rotbelly_player_scoped_reflexive_body_fails_closed() {
    let parsed = parse_oracle_text(
        "When this creature enters, you may sacrifice up to three Zombies. When you sacrifice one or more Zombies this way, each opponent sacrifices that many creatures of their choice.",
        "Ravenous Rotbelly",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let execute = parsed.triggers[0].execute.as_deref().expect("ETB body");
    assert!(
        matches!(execute.effect.as_ref(), Effect::Sacrifice { .. }),
        "reach guard: the producer is the printed sacrifice"
    );
    assert_eq!(
        unimplemented_name(execute).as_deref(),
        Some("zone_change_reflexive_scoped_body"),
        "{execute:?}"
    );
}

/// Nyssa of Traken's attack-trigger body: "that many" bounds both the
/// reflexive's target count (chosen as it goes on the stack) and its draw.
#[test]
fn nyssa_body_that_many_bounds_targets_and_draw() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe",
            false,
            "Sacrifice any number of artifacts. When you sacrifice one or more artifacts this way, tap up to that many target creatures and draw that many cards.",
        )
        .id();
    let a1 = s.add_creature(P0, "Relic One", 0, 1).as_artifact().id();
    let a2 = s.add_creature(P0, "Relic Two", 0, 1).as_artifact().id();
    let o1 = s.add_creature(P1, "Opp One", 1, 1).id();
    let o2 = s.add_creature(P1, "Opp Two", 1, 1).id();
    s.add_creature(P1, "Opp Three", 1, 1);
    s.with_library_top(P0, &["Draw One", "Draw Two", "Draw Three"]);
    let mut r = s.build();
    let hand_before = r.state().players[0].hand.len();
    cast(&mut r, spell);
    let out = drive(
        &mut r,
        Plan::new(a1)
            .picks(vec![a1, a2])
            .target(vec![TargetRef::Object(o1), TargetRef::Object(o2)]),
    );
    assert_eq!(
        out.target_prompts,
        vec![Zone::Graveyard],
        "targets after the sacrifice"
    );
    assert!(
        out.triggers_on_stack >= 1,
        "the reflexive went on the stack"
    );
    assert!(r.state().objects[&o1].tapped && r.state().objects[&o2].tapped);
    assert_eq!(
        r.state().players[0].hand.len(),
        hand_before - 1 + 2,
        "drew that many (two); the cast spell left the hand"
    );
}

// ---------------------------------------------------------------------------
// Battlefield-entry heads
// ---------------------------------------------------------------------------

/// Adaptive Armorer / Masterpiece Vault shape ("When an Equipment enters this
/// way, attach it to target creature you control"), from a production
/// put-onto-battlefield producer: the moved Equipment is the attachment, the
/// host is the reflexive's own target chosen after the entry.
#[test]
fn entered_equipment_attaches_to_the_reflexive_target() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe",
            false,
            "Put an Equipment card from your hand onto the battlefield. When an Equipment enters this way, attach it to target creature you control.",
        )
        .id();
    let equipment = s
        .add_artifact_to_hand_from_oracle(P0, "Hand Equipment", "Equip {2}")
        .with_subtypes(vec!["Equipment"])
        .id();
    // A second Equipment in hand, so the put-onto-the-battlefield instruction
    // pauses for its choice and the reflexive is created on the resumed path.
    let other_equipment = s
        .add_artifact_to_hand_from_oracle(P0, "Other Hand Equipment", "Equip {2}")
        .with_subtypes(vec!["Equipment"])
        .id();
    let bear = s.add_creature(P0, "Bear", 2, 2).id();
    s.add_creature(P0, "Cub", 1, 1);
    let mut r = s.build();
    cast(&mut r, spell);
    let out = drive(
        &mut r,
        Plan::new(equipment)
            .picks(vec![equipment])
            .target(vec![TargetRef::Object(bear)]),
    );
    assert_eq!(
        out.zone_choices.len(),
        1,
        "reach: the producer paused for its choice"
    );
    assert_eq!(
        r.state().objects[&other_equipment].zone,
        Zone::Hand,
        "only the chosen Equipment moved"
    );
    assert_eq!(
        out.target_prompts,
        vec![Zone::Battlefield],
        "host chosen after entry"
    );
    assert_eq!(
        r.state().objects[&equipment].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(bear))
    );
}

/// Probe boards 1 and 2 (fail closed): Adaptive Armorer and Masterpiece Vault
/// put the drafted card onto the battlefield through the spellbook-draft
/// resume, which does not publish the zone-change ledger the gate reads, so
/// the reflexive could never be created; both printed cards are not claimed.
/// (Masterpiece Vault's "up to one target" host is moot behind that.)
#[test]
fn spellbook_draft_entry_reflexives_fail_closed() {
    for (name, text, types) in [
        (
            "Adaptive Armorer",
            "Vigilance\nWhen this creature enters, draft a card from Adaptive Armorer's spellbook and put it onto the battlefield. When an Equipment enters this way, attach it to target creature you control.",
            "Creature",
        ),
        (
            "Masterpiece Vault",
            "When this artifact is put into a graveyard from the battlefield, draft a card from Masterpiece Vault's spellbook and put it onto the battlefield. When an Equipment enters this way, attach it to up to one target creature you control.\n{5}: Sacrifice this artifact.",
            "Artifact",
        ),
    ] {
        let parsed = parse_oracle_text(text, name, &[], &[types.to_string()], &[]);
        let execute = parsed
            .triggers
            .iter()
            .find_map(|trigger| trigger.execute.as_deref())
            .expect("trigger body");
        assert!(
            matches!(execute.effect.as_ref(), Effect::DraftFromSpellbook { .. }),
            "reach guard: {name}'s producer is the spellbook draft"
        );
        assert_eq!(
            unimplemented_name(execute).as_deref(),
            Some("zone_change_reflexive_event_source"),
            "{name}: {execute:?}"
        );
    }
}

/// CR 603.12a: a move whose target is handed over by an earlier instruction
/// (an inherited parent target, "them") carries no cardinality the parser can
/// prove: "Tap two target creatures. Exile them." moves both creatures, and
/// "Tap target creature. Reveal the top two cards of your library. Exile them."
/// moves both revealed cards. A single-occurrence head after such a move would
/// trigger once per object while the creation gate makes one trigger, so it
/// fails closed, conservatively including the singular "return it" form. The
/// aggregate head stays supported and triggers once.
#[test]
fn single_occurrence_head_after_an_inherited_target_move_fails_closed() {
    for (text, earlier) in [
        (
            "Tap two target creatures. Exile them. When you exile a creature this way, you gain 1 life.",
            "the two-target tap",
        ),
        (
            "Tap target creature. Reveal the top two cards of your library. Exile them. When you exile a card this way, you gain 1 life.",
            "the reveal of two cards",
        ),
        (
            "Exile target creature, then return it to the battlefield under its owner's control. When a creature enters this way, you gain 1 life.",
            "the single-target exile",
        ),
    ] {
        let single = spell_ability(text, "Probe");
        let mut node = Some(&single);
        let mut producer = None;
        while let Some(current) = node {
            if matches!(current.effect.as_ref(), Effect::Unimplemented { .. }) {
                break;
            }
            producer = Some(current);
            node = current.sub_ability.as_deref();
        }
        let producer = producer.expect("a producer before the head");
        assert!(
            matches!(producer.effect.as_ref(), Effect::ChangeZone { .. }),
            "reach guard: the head follows a move of what {earlier} handed over: {single:?}"
        );
        assert_eq!(
            unimplemented_name(&single).as_deref(),
            Some("zone_change_reflexive_occurrence"),
            "{single:?}"
        );
    }
    let reveal = spell_ability(
        "Tap target creature. Reveal the top two cards of your library. Exile them. When you exile a card this way, you gain 1 life.",
        "Probe",
    );
    assert!(
        matches!(
            reveal.sub_ability.as_deref().map(|sub| sub.effect.as_ref()),
            Some(Effect::RevealTop { .. } | Effect::Dig { .. })
        ),
        "reach guard: the reveal-two instruction: {reveal:?}"
    );

    let aggregate = "Tap two target creatures. Exile them. When you exile one or more creatures this way, you gain 1 life.";
    assert_supported_zone_change_reflexive(&spell_ability(aggregate, "Probe"));
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Probe", false, aggregate)
        .id();
    let first = s.add_creature(P1, "First", 2, 2).id();
    let second = s.add_creature(P1, "Second", 2, 2).id();
    let mut r = s.build();
    let life = r.life(P0);
    cast(&mut r, spell);
    let out = drive(
        &mut r,
        Plan::new(first).target(vec![TargetRef::Object(first), TargetRef::Object(second)]),
    );
    assert_eq!(r.state().objects[&first].zone, Zone::Exile, "reach: exiled");
    assert_eq!(
        r.state().objects[&second].zone,
        Zone::Exile,
        "reach: exiled"
    );
    assert_eq!(out.triggers_on_stack, 1, "one reflexive on the stack");
    assert_eq!(r.life(P0), life + 1, "the aggregate head triggers once");

    let revealed = "Tap target creature. Reveal the top two cards of your library. Exile them. When you exile one or more cards this way, you gain 1 life.";
    assert_supported_zone_change_reflexive(&spell_ability(revealed, "Probe"));
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Probe", false, revealed)
        .id();
    let top_one = s.add_card_to_library_top(P0, "Top One");
    let top_two = s.add_card_to_library_top(P0, "Top Two");
    let opp = s.add_creature(P1, "Opp Creature", 2, 2).id();
    let mut r = s.build();
    let life = r.life(P0);
    cast(&mut r, spell);
    let out = drive(&mut r, Plan::new(opp).target(vec![TargetRef::Object(opp)]));
    assert!(r.state().objects[&opp].tapped, "reach: the tap resolved");
    assert_eq!(
        r.state().objects[&top_one].zone,
        Zone::Exile,
        "both revealed cards exiled"
    );
    assert_eq!(
        r.state().objects[&top_two].zone,
        Zone::Exile,
        "both revealed cards exiled"
    );
    assert_eq!(out.triggers_on_stack, 1, "one reflexive on the stack");
    assert_eq!(r.life(P0), life + 1, "the aggregate head triggers once");
}

/// Probe board 4 (CR 603.12a): "When you exile a card this way" counts one
/// occurrence, so after an instruction that exiles two cards it would trigger
/// twice, while the creation gate makes one trigger: fails closed. The
/// aggregate head ("one or more cards") is one event and triggers once.
#[test]
fn single_occurrence_head_after_a_multi_card_producer_fails_closed() {
    let single = spell_ability(
        "Exile the top two cards of your library. When you exile a card this way, you gain 1 life.",
        "Probe",
    );
    assert!(
        matches!(single.effect.as_ref(), Effect::ExileTop { .. }),
        "reach guard: the two-card exile"
    );
    assert_eq!(
        unimplemented_name(&single).as_deref(),
        Some("zone_change_reflexive_occurrence"),
        "{single:?}"
    );

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe",
            false,
            "Exile the top two cards of your library. When you exile one or more cards this way, you gain 1 life.",
        )
        .id();
    let first = s.add_card_to_library_top(P0, "First");
    s.add_card_to_library_top(P0, "Second");
    let mut r = s.build();
    let life = r.life(P0);
    cast(&mut r, spell);
    let out = drive(&mut r, Plan::new(first));
    assert_eq!(r.state().objects[&first].zone, Zone::Exile, "reach: exiled");
    assert_eq!(out.triggers_on_stack, 1, "one reflexive on the stack");
    assert_eq!(r.life(P0), life + 1, "the aggregate head triggers once");
}

/// CR 603.12: the reflexive survives a save/restore taken while its PRODUCER
/// is paused on the discard choice; after the restore the choice is answered
/// and the reflexive is created and targets as usual.
#[test]
fn reflexive_survives_save_and_restore_at_the_producers_discard_choice() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Glacial Dragonhunt", false, GLACIAL_DRAGONHUNT)
        .id();
    let three = s
        .add_spell_to_hand(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    s.add_spell_to_hand(P0, "Two Drop", false)
        .with_mana_cost(ManaCost::generic(2));
    s.add_card_to_library_top(P0, "Library Card");
    let opp = s.add_creature(P1, "Opp Giant", 4, 4).id();
    s.add_creature(P1, "Opp Cub", 1, 1);
    let mut r = s.build();
    cast(&mut r, spell);
    for _ in 0..8 {
        match r.state().waiting_for.clone() {
            WaitingFor::DiscardChoice { .. } => break,
            WaitingFor::OptionalEffectChoice { .. } => {
                r.act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional discard");
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    assert!(
        matches!(r.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "reach: the producer is paused on its choice"
    );
    let json = serde_json::to_string(&PersistedGameState::capture(r.state().clone()))
        .expect("state serializes");
    let state = serde_json::from_str::<PersistedGameState>(&json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restored state is publishable");
    let mut restored = GameRunner::from_state(state);
    let out = drive(
        &mut restored,
        Plan::new(three)
            .picks(vec![three])
            .target(vec![TargetRef::Object(opp)]),
    );
    assert_eq!(out.target_prompts, vec![Zone::Graveyard]);
    assert_eq!(restored.state().objects[&opp].damage_marked, 3);
}

/// CR 603.12 + CR 603.3d (fail closed): printed class members the reflexive
/// stack object cannot carry — Hans Eriksson (its event is put onto the
/// battlefield by a branch of the reveal, and "it fights" was left as the
/// parent's target), Three Dog ("a copy of that Aura" has no copy source), and
/// Saruman of Many Colors ("each opponent mills" resolves its gate once per
/// opponent).
#[test]
fn reflexive_with_an_object_the_stack_cannot_carry_fails_closed() {
    /// Every node of the body, through both branches.
    fn nodes(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
        let mut out = vec![def];
        for child in [def.sub_ability.as_deref(), def.else_ability.as_deref()]
            .into_iter()
            .flatten()
        {
            out.extend(nodes(child));
        }
        out
    }
    fn enters_battlefield(e: &Effect) -> bool {
        matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        )
    }
    fn sacrifices(e: &Effect) -> bool {
        matches!(e, Effect::Sacrifice { .. })
    }
    fn mills(e: &Effect) -> bool {
        matches!(e, Effect::Mill { .. })
    }
    /// (card name, expected gap, printed text, producer shape)
    type Case = (
        &'static str,
        &'static str,
        &'static str,
        fn(&Effect) -> bool,
    );
    let cases: [Case; 3] = [
        (
            "Hans Eriksson",
            "zone_change_reflexive_event_source",
            "Whenever Hans Eriksson attacks, reveal the top card of your library. If it's a creature card, put it onto the battlefield tapped and attacking defending player or a planeswalker they control. Otherwise, put that card into your hand. When you put a creature card onto the battlefield this way, it fights Hans Eriksson.",
            enters_battlefield,
        ),
        (
            "Three Dog, Galaxy News DJ",
            "zone_change_reflexive_unbound_referent",
            "Whenever you attack, you may pay {2} and sacrifice an Aura attached to Three Dog. When you sacrifice an Aura this way, for each other attacking creature you control, create a token that's a copy of that Aura attached to that creature.",
            sacrifices,
        ),
        (
            "Saruman of Many Colors",
            "zone_change_reflexive_event_source",
            "Whenever you cast your second spell each turn, each opponent mills two cards. When one or more cards are milled this way, exile target enchantment, instant, or sorcery card with equal or lesser mana value than that spell from an opponent's graveyard. Copy the exiled card. You may cast the copy without paying its mana cost.",
            mills,
        ),
    ];
    for (name, expected_gap, text, producer) in cases {
        let parsed = parse_oracle_text(text, name, &[], &["Creature".to_string()], &[]);
        let body = parsed
            .triggers
            .iter()
            .find_map(|trigger| trigger.execute.as_deref())
            .expect("trigger body");
        let all = nodes(body);
        assert!(
            all.iter().any(|node| producer(node.effect.as_ref())),
            "{name}: reach guard, the producer is in the trigger body: {body:?}"
        );
        assert!(
            all.iter().any(|node| matches!(
                node.effect.as_ref(),
                Effect::Unimplemented { name: gap, description: Some(head) }
                    if gap == expected_gap && head.contains("this way")
            )),
            "{name}: the head clause fails closed as {expected_gap}: {body:?}"
        );
        assert!(
            all.iter().all(|node| !node
                .condition
                .as_ref()
                .is_some_and(|condition| condition.has_when_you_do_marker())),
            "{name}: no reflexive gate is claimed: {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Fail-closed shapes
// ---------------------------------------------------------------------------

/// CR 608.2c: the gate reads the zone change the instruction just before it
/// published. A different instruction in between ("then draw", "then seek"),
/// a cost-shaped discard under a sacrifice, or a delayed sacrifice leaves the
/// gate reading the wrong cards, so the reflexive is not claimed.
#[test]
fn reflexive_whose_event_is_not_the_preceding_instruction_fails_closed() {
    for (name, text) in [
        (
            "Fiery Encore",
            "Discard a card, then draw a card. When you discard a nonland card this way, Fiery Encore deals damage equal to that card's mana value to target creature or planeswalker.",
        ),
        (
            "Flamethrower Sonata",
            "Discard a card, then draw a card. When you discard an instant or sorcery card this way, Flamethrower Sonata deals damage equal to that card's mana value to target creature or planeswalker you don't control.",
        ),
        (
            "Summon: Kujata",
            "Discard a card, then draw two cards. When you discard a card this way, this creature deals damage equal to that card's mana value to each opponent.",
        ),
    ] {
        let def = spell_ability(text, name);
        assert_eq!(
            unimplemented_name(&def).as_deref(),
            Some("zone_change_reflexive_event_source"),
            "{name}: {def:?}"
        );
    }
    for (name, text, types) in [
        (
            "Chomping Mastasaur",
            "Whenever this creature enters or attacks, discard a card, then seek a nonland card. When you discard a card this way, this creature deals damage equal to the discarded card's mana value to any target.",
            "Creature",
        ),
        (
            "Argentum Masticore",
            "At the beginning of your upkeep, sacrifice this creature unless you discard a card. When you discard a card this way, destroy target nonland permanent an opponent controls with mana value less than or equal to the mana value of the discarded card.",
            "Creature",
        ),
        (
            "Goblin Crash Pilot",
            "Whenever this creature crews a Vehicle, that Vehicle gains haste until end of turn. At the beginning of the next end step, sacrifice that Vehicle. When you sacrifice a creature this way, it deals damage equal to its power to any target.",
            "Creature",
        ),
    ] {
        let parsed = parse_oracle_text(text, name, &[], &[types.to_string()], &[]);
        let execute = parsed.triggers[0].execute.as_deref().expect("trigger body");
        assert_eq!(
            unimplemented_name(execute).as_deref(),
            Some("zone_change_reflexive_event_source"),
            "{name}: {execute:?}"
        );
    }
}

/// CR 603.3d: a reflexive created by a zone change has no announced parent
/// target, so a body whose recipient was rebound to the parent's target can
/// never choose one. Not claimed.
#[test]
fn reflexive_with_an_unbound_recipient_fails_closed() {
    let parsed = parse_oracle_text(
        "When this creature enters, draw two cards, then discard two cards. When you discard one or more nonland cards this way, tap up to that many target creatures and put a stun counter on each of them.",
        "Seasoned Cryomancer",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let execute = parsed.triggers[0].execute.as_deref().expect("ETB body");
    assert!(
        matches!(execute.effect.as_ref(), Effect::Draw { .. }),
        "reach guard: the printed ETB body"
    );
    assert_eq!(
        unimplemented_name(execute).as_deref(),
        Some("zone_change_reflexive_unbound_referent")
    );
}

/// CR 603.12 + CR 603.4: a zone-change intervening "if" on a reflexive would
/// share the root with a zone-change trigger event; the role is not
/// representable, so it fails closed instead of being settled as the event.
#[test]
fn zone_change_intervening_if_on_a_reflexive_fails_closed() {
    let def = spell_ability(
        "Exile target card from a graveyard. When you do, if a creature card was exiled this way, you gain 3 life.",
        "Probe",
    );
    assert_eq!(
        unimplemented_name(&def).as_deref(),
        Some("when_you_do_guard"),
        "{def:?}"
    );
}
