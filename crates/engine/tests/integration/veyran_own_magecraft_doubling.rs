//! CR 603.2d + CR 707.10: Veyran, Voice of Duality doubles its own Magecraft.
//!
//! Oracle: "Magecraft — Whenever you cast or copy an instant or sorcery spell,
//! Veyran gets +1/+1 until end of turn. If you casting or copying an instant or
//! sorcery spell causes a triggered ability of a permanent you control to
//! trigger, that ability triggers an additional time."
//!
//! Veyran is "a permanent you control", so its own Magecraft is in the
//! doubler's scope and triggers twice per instant or sorcery its controller
//! casts or copies (https://github.com/phase-rs/phase/issues/8838). A
//! doubler's own triggers are decided by its scope and cause alone: "another"
//! scopes (Harmonic Prodigy), "a ... spell" scopes (Echoes of Eternity) and
//! "another permanent entering" causes (The Fish Brewer) leave the doubler's
//! own triggers alone, and a typed cause (Valiant Emberkin) doubles only what
//! that event causes.
//!
//! Signals: a creature's power counts its resolved pump/counter instances;
//! Witherbloom Apprentice's life gain counts its resolved Magecraft instances.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const VEYRAN: &str = "Magecraft — Whenever you cast or copy an instant or sorcery spell, Veyran gets +1/+1 until end of turn.\nIf you casting or copying an instant or sorcery spell causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";
const WITHERBLOOM_APPRENTICE: &str = "Magecraft — Whenever you cast or copy an instant or sorcery spell, each opponent loses 1 life and you gain 1 life.";
const HARMONIC_PRODIGY: &str = "Prowess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)\nIf a triggered ability of a Shaman or another Wizard you control triggers, that ability triggers an additional time.";
const STORMCHASER_MAGE: &str = "Flying, haste\nProwess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)";
const ECHOES_OF_ETERNITY: &str = "If a triggered ability of a colorless spell you control or another colorless permanent you control triggers, that ability triggers an additional time.\nWhenever you cast a colorless spell, copy it. You may choose new targets for the copy. (A copy of a permanent spell becomes a token.)";
const VALIANT_EMBERKIN: &str = "Haste\nWhenever this creature enters or attacks, target creature you control perpetually gets +1/+0.\nIf a creature you control becoming the target of a spell or ability causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";
const HEARTFIRE_HERO: &str = "Valiant — Whenever this creature becomes the target of a spell or ability you control for the first time each turn, put a +1/+1 counter on it.\nWhen this creature dies, it deals damage equal to its power to each opponent.";
const THE_FISH_BREWER: &str = "When The Fish Brewer enters the battlefield or attacks, create a 1/1 Fish creature token that's all colors.\nIf another permanent entering the battlefield causes a triggered ability of a permanent you control to trigger, tap any number of Fish you control. That ability triggers an additional time for each Fish tapped this way.";
/// No target: Twincast copying it takes the copy resolver's immediate drain.
const QUICK_STUDY: &str = "Draw two cards.";
/// Targeted: Twincast copying it takes the CR 707.10c retarget walk, whose
/// copy observers drain after the walk completes.
const INSPIRATION: &str = "Target player draws two cards.";
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const DESOLATION_TWIN: &str =
    "When you cast this spell, create a 10/10 colorless Eldrazi creature token.";
const FELIX_FIVE_BOOTS: &str = "If a creature you control dealing combat damage to a player causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";
const MURDER: &str = "Destroy target creature.";
const JABS: &str = "If a triggered ability of a permanent you control but don't own triggers, that ability triggers an additional time.";
const SCROLL_THIEF: &str = "Whenever this creature deals combat damage to a player, draw a card.";
const PROFESSIONAL_FACE_BREAKER: &str = "Menace
Whenever one or more creatures you control deal combat damage to a player, create a Treasure token.
Sacrifice a Treasure: Exile the top card of your library. You may play that card this turn.";

fn scenario() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for i in 0..30 {
        s.add_card_to_library_top(P0, &format!("P0 Lib {i}"));
    }
    s
}

fn add_veyran(s: &mut GameScenario) -> ObjectId {
    s.add_creature_from_oracle(P0, "Veyran, Voice of Duality", 2, 2, VEYRAN)
        .with_subtypes(vec!["Efreet", "Wizard"])
        .id()
}

fn add_apprentice(s: &mut GameScenario) -> ObjectId {
    s.add_creature_from_oracle(P0, "Witherbloom Apprentice", 2, 2, WITHERBLOOM_APPRENTICE)
        .with_subtypes(vec!["Human", "Druid"])
        .id()
}

fn add_echoes(s: &mut GameScenario) {
    s.add_enchantment_from_oracle(P0, "Echoes of Eternity", ECHOES_OF_ETERNITY)
        .with_subtypes(vec!["Eldrazi"]);
}

fn add_instant(s: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, name, true, text).id()
}

fn power(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id]
        .power
        .expect("creature has power")
}

fn life(runner: &GameRunner) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .life
}

fn count_copies(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::SpellCopied { .. }))
        .count()
}

fn cast(runner: &mut GameRunner, spell: ObjectId) -> Vec<GameEvent> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must be accepted")
        .events
}

/// Answer prompts (a target: `prefer` when legal, else the first legal one);
/// resolve the stack fully when `resolve` is set, else stop at the first
/// priority window. Returns the events of every action taken.
fn drive_preferring(
    runner: &mut GameRunner,
    resolve: bool,
    prefer: Option<TargetRef>,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..256 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                let target = prefer
                    .clone()
                    .filter(|t| legal.contains(t))
                    .or_else(|| legal.first().cloned());
                GameAction::ChooseTarget { target }
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::CopyRetarget { .. } => GameAction::KeepAllCopyTargets,
            WaitingFor::Priority { .. } => {
                if !resolve || runner.state().stack.is_empty() {
                    return events;
                }
                GameAction::PassPriority
            }
            other => panic!("unexpected prompt: {other:?}"),
        };
        events.extend(runner.act(action).expect("action accepted").events);
    }
    panic!("drive did not settle");
}

fn drive(runner: &mut GameRunner, resolve: bool) -> Vec<GameEvent> {
    drive_preferring(runner, resolve, None)
}

/// Cast `first`, respond with Twincast (it targets `first`, the only spell on
/// the stack), and resolve everything. Returns how many `SpellCopied` events
/// the copy produced.
fn cast_then_twincast(runner: &mut GameRunner, first: ObjectId, twincast: ObjectId) -> usize {
    cast(runner, first);
    drive(runner, false);
    let mut events = cast(runner, twincast);
    events.extend(drive(runner, true));
    count_copies(&events)
}

/// Casting an instant gives exactly two instances of Veyran's own Magecraft.
/// Revert-to-red: restoring the blanket self-exclusion in `doubler_applies`
/// leaves one instance (power 3).
#[test]
fn veyran_doubles_its_own_magecraft_on_cast() {
    let mut s = scenario();
    let veyran = add_veyran(&mut s);
    let spell = add_instant(&mut s, "Quick Study", QUICK_STUDY);
    let mut runner = s.build();

    cast(&mut runner, spell);
    drive(&mut runner, true);

    assert_eq!(
        power(&runner, veyran),
        4,
        "2/2 Veyran + two Magecraft instances"
    );
}

/// Copying gives exactly two instances of Veyran's own Magecraft from one
/// `SpellCopied`, on the copy resolver's immediate path (Twincast copying an
/// untargeted spell). Casts: Quick Study (+2) and Twincast (+2); the copy
/// (+2). Revert-to-red: the self-exclusion leaves +1 per cast; a second
/// collection of the same `SpellCopied` adds instances for the copy.
#[test]
fn veyran_doubles_its_own_magecraft_on_copy() {
    let mut s = scenario();
    let veyran = add_veyran(&mut s);
    let spell = add_instant(&mut s, "Quick Study", QUICK_STUDY);
    let twincast = add_instant(&mut s, "Twincast", TWINCAST);
    let mut runner = s.build();

    let copies = cast_then_twincast(&mut runner, spell, twincast);

    assert_eq!(copies, 1, "one copy, one SpellCopied");
    assert_eq!(
        power(&runner, veyran),
        8,
        "2 + 2 (cast) + 2 (cast) + 2 (copy)"
    );
}

/// Same through the CR 707.10c retarget walk (Twincast copying a targeted
/// spell), whose copy observers drain after the walk completes.
#[test]
fn veyran_doubles_its_own_magecraft_on_retargetable_copy() {
    let mut s = scenario();
    let veyran = add_veyran(&mut s);
    let spell = add_instant(&mut s, "Inspiration", INSPIRATION);
    let twincast = add_instant(&mut s, "Twincast", TWINCAST);
    let mut runner = s.build();

    let copies = cast_then_twincast(&mut runner, spell, twincast);

    assert_eq!(copies, 1, "one copy, one SpellCopied");
    assert_eq!(
        power(&runner, veyran),
        8,
        "2 + 2 (cast) + 2 (cast) + 2 (copy)"
    );
}

/// CR 603.2c: without a doubler, one copy is one Magecraft trigger: a
/// `SpellCopied` is collected once. Three events (two casts, one copy) gain 3
/// life. Revert-to-red: dropping the copy resolver's claim gains 4.
#[test]
fn copied_spell_triggers_magecraft_once_without_a_doubler() {
    for (name, text) in [("Quick Study", QUICK_STUDY), ("Inspiration", INSPIRATION)] {
        let mut s = scenario();
        add_apprentice(&mut s);
        let spell = add_instant(&mut s, name, text);
        let twincast = add_instant(&mut s, "Twincast", TWINCAST);
        let mut runner = s.build();
        let before = life(&runner);

        cast_then_twincast(&mut runner, spell, twincast);

        assert_eq!(life(&runner) - before, 3, "{name}: two casts + one copy");
    }
}

/// Another Magecraft permanent you control is doubled on both cast and copy.
#[test]
fn veyran_doubles_another_magecraft_permanent_on_cast_and_copy() {
    for (name, text) in [("Quick Study", QUICK_STUDY), ("Inspiration", INSPIRATION)] {
        let mut s = scenario();
        add_veyran(&mut s);
        add_apprentice(&mut s);
        let spell = add_instant(&mut s, name, text);
        let twincast = add_instant(&mut s, "Twincast", TWINCAST);
        let mut runner = s.build();
        let before = life(&runner);

        cast_then_twincast(&mut runner, spell, twincast);

        assert_eq!(
            life(&runner) - before,
            6,
            "{name}: three events, each doubled"
        );
    }
}

/// CR 603.2d: two Veyrans each add one additional time, so each Veyran's own
/// Magecraft triggers three times per cast or copy.
#[test]
fn two_veyrans_trigger_each_magecraft_three_times() {
    let mut s = scenario();
    let first = add_veyran(&mut s);
    let second = add_veyran(&mut s);
    let spell = add_instant(&mut s, "Quick Study", QUICK_STUDY);
    let twincast = add_instant(&mut s, "Twincast", TWINCAST);
    let mut runner = s.build();

    cast_then_twincast(&mut runner, spell, twincast);

    // Three events (two casts, one copy), three instances each.
    assert_eq!(power(&runner, first), 2 + 9);
    assert_eq!(power(&runner, second), 2 + 9);
}

/// A doubler that says "another" skips its own triggers through its scope:
/// Harmonic Prodigy (a Wizard, not a Shaman) doubles another Wizard's
/// (Stormchaser Mage's) prowess but not its own.
#[test]
fn another_scoped_doubler_does_not_double_itself() {
    let mut s = scenario();
    let prodigy = s
        .add_creature(P0, "Harmonic Prodigy", 1, 3)
        .with_subtypes(vec!["Human", "Wizard"])
        .from_oracle_text_with_keywords(&["Prowess"], HARMONIC_PRODIGY)
        .id();
    let mage = s
        .add_creature(P0, "Stormchaser Mage", 1, 3)
        .with_subtypes(vec!["Human", "Wizard"])
        .from_oracle_text_with_keywords(&["Flying", "Haste", "Prowess"], STORMCHASER_MAGE)
        .id();
    let spell = add_instant(&mut s, "Quick Study", QUICK_STUDY);
    let mut runner = s.build();

    cast(&mut runner, spell);
    drive(&mut runner, true);

    assert_eq!(
        power(&runner, prodigy),
        2,
        "Prodigy's own prowess triggers once"
    );
    assert_eq!(
        power(&runner, mage),
        3,
        "another Wizard's prowess is doubled"
    );
}

/// CR 603.2d: Echoes of Eternity scopes its doubling to "a colorless spell you
/// control or another colorless permanent you control". Echoes is a colorless
/// permanent, not a spell, and not "another", so its own copy trigger triggers
/// once: one colorless spell cast, one copy. Revert-to-red: without the stack
/// scope on the "spell" disjunct, that disjunct matches Echoes on the
/// battlefield and the spell is copied twice.
#[test]
fn echoes_of_eternity_does_not_double_its_own_trigger() {
    let mut s = scenario();
    add_echoes(&mut s);
    let spell = add_instant(&mut s, "Quick Study", QUICK_STUDY);
    let mut runner = s.build();

    let mut events = cast(&mut runner, spell);
    events.extend(drive(&mut runner, true));

    assert_eq!(
        count_copies(&events),
        1,
        "Echoes' own copy trigger triggers once"
    );
}

/// CR 603.2d + CR 112.1: the stack-scoped "colorless spell you control"
/// disjunct still doubles a colorless spell's own cast trigger. Desolation
/// Twin's cast trigger triggers twice, so two Eldrazi tokens; the copy Echoes
/// makes is not cast (CR 707.10) and makes none.
#[test]
fn echoes_of_eternity_doubles_a_colorless_spells_cast_trigger() {
    let mut s = scenario();
    add_echoes(&mut s);
    let twin = s
        .add_creature_to_hand_from_oracle(P0, "Desolation Twin", 10, 10, DESOLATION_TWIN)
        .with_subtypes(vec!["Eldrazi"])
        .id();
    let mut runner = s.build();

    cast(&mut runner, twin);
    drive(&mut runner, true);

    let tokens = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].name == "Eldrazi")
        .count();
    assert_eq!(tokens, 2, "the spell's cast trigger is doubled");
}

/// CR 603.2d: Valiant Emberkin doubles only triggers that a creature you
/// control becoming the target causes. Its own enters trigger is caused by it
/// entering, so it triggers once: Emberkin (1/2), the only creature you
/// control, perpetually gets +1/+0 once. Revert-to-red: with the cause parsed
/// as `Any`, the enters trigger is doubled (power 3).
#[test]
fn valiant_emberkin_does_not_double_its_own_enters_trigger() {
    let mut s = scenario();
    let emberkin = s
        .add_creature_to_hand(P0, "Valiant Emberkin", 1, 2)
        .with_subtypes(vec!["Mouse", "Glimmer"])
        .from_oracle_text_with_keywords(&["Haste"], VALIANT_EMBERKIN)
        .id();
    let mut runner = s.build();

    cast(&mut runner, emberkin);
    drive(&mut runner, true);

    assert_eq!(runner.state().objects[&emberkin].zone, Zone::Battlefield);
    assert_eq!(power(&runner, emberkin), 2, "one enters trigger resolved");
}

/// CR 603.2d + CR 115.1: a creature you control becoming the target of a
/// spell you control is Emberkin's cause, so Heartfire Hero's valiant trigger
/// triggers twice when Giant Growth targets it: 1/1 + two counters + 3.
#[test]
fn valiant_emberkin_doubles_a_becomes_target_trigger() {
    let mut s = scenario();
    s.add_creature(P0, "Valiant Emberkin", 1, 2)
        .with_subtypes(vec!["Mouse", "Glimmer"])
        .from_oracle_text_with_keywords(&["Haste"], VALIANT_EMBERKIN);
    let hero = s
        .add_creature_from_oracle(P0, "Heartfire Hero", 1, 1, HEARTFIRE_HERO)
        .with_subtypes(vec!["Mouse", "Soldier"])
        .id();
    let growth = add_instant(&mut s, "Giant Growth", GIANT_GROWTH);
    let mut runner = s.build();

    cast(&mut runner, growth);
    drive_preferring(&mut runner, true, Some(TargetRef::Object(hero)));

    assert_eq!(power(&runner, hero), 1 + 2 + 3);
}

/// Emberkin's typed cause no longer doubles triggers that a cast causes:
/// Witherbloom Apprentice's Magecraft triggers once for an untargeted
/// instant. Revert-to-red: with the cause parsed as `Any`, it gains 2.
#[test]
fn valiant_emberkin_does_not_double_unrelated_triggers() {
    let mut s = scenario();
    s.add_creature(P0, "Valiant Emberkin", 1, 2)
        .with_subtypes(vec!["Mouse", "Glimmer"])
        .from_oracle_text_with_keywords(&["Haste"], VALIANT_EMBERKIN);
    add_apprentice(&mut s);
    let spell = add_instant(&mut s, "Quick Study", QUICK_STUDY);
    let mut runner = s.build();
    let before = life(&runner);

    cast(&mut runner, spell);
    drive(&mut runner, true);

    assert_eq!(life(&runner) - before, 1);
}

/// CR 603.2d: The Fish Brewer doubles triggers caused by "another permanent
/// entering", so its own enters trigger, caused by its own entering, triggers
/// once: one Fish. Revert-to-red: dropping "another" from the cause makes the
/// Brewer double its own enters trigger (two Fish).
#[test]
fn the_fish_brewer_does_not_double_its_own_enters_trigger() {
    let mut s = scenario();
    let brewer = s
        .add_creature_to_hand_from_oracle(P0, "The Fish Brewer", 4, 4, THE_FISH_BREWER)
        .with_subtypes(vec!["Human", "Wizard"])
        .id();
    let mut runner = s.build();

    cast(&mut runner, brewer);
    drive(&mut runner, true);

    let fish = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].name == "Fish")
        .count();
    assert_eq!(runner.state().objects[&brewer].zone, Zone::Battlefield);
    assert_eq!(fish, 1, "one enters trigger, one Fish");
}

/// CR 603.2d + CR 702.21a: Felix Five-Boots doubles only triggers that a
/// creature you control dealing combat damage to a player causes. An
/// opponent's spell targeting Felix triggers its ward once. Revert-to-red:
/// with the cause parsed as `Any`, Felix doubles its own ward (two triggers).
#[test]
fn felix_five_boots_does_not_double_its_own_ward() {
    let mut s = scenario();
    let felix = add_felix(&mut s, P1);
    let murder = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    let commit = runner.cast(murder).target_object(felix).commit();
    let ward_triggers = commit
        .state()
        .stack
        .iter()
        .filter(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == felix)
        })
        .count();

    assert_eq!(ward_triggers, 1, "Felix's ward triggers once");
}

fn add_felix(s: &mut GameScenario, player: engine::types::player::PlayerId) -> ObjectId {
    s.add_creature_from_oracle(player, "Felix Five-Boots", 5, 4, FELIX_FIVE_BOOTS)
        .with_subtypes(vec!["Ooze", "Rogue"])
        .with_keyword(Keyword::Menace)
        .with_keyword(Keyword::Ward(WardCost::Mana(ManaCost::generic(2))))
        .id()
}

/// Attack P1 with `attackers` unblocked and resolve combat and every trigger.
fn attack_unblocked(runner: &mut GameRunner, attackers: &[ObjectId]) {
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("attackers declared");
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. }
                if runner.state().stack.is_empty()
                    && runner.state().phase == Phase::PostCombatMain =>
            {
                return;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("combat did not settle");
}

fn hand_size(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .len()
}

/// CR 603.2d + CR 510.2: a creature you control dealing combat damage to a
/// player is Felix's cause, so Scroll Thief's own combat-damage trigger
/// triggers twice: two cards. Revert-to-red: a Felix arm that never matches
/// the per-source combat `DamageDealt` leaves one card.
#[test]
fn felix_five_boots_doubles_a_per_creature_combat_damage_trigger() {
    let mut s = scenario();
    add_felix(&mut s, P0);
    let thief = s
        .add_creature_from_oracle(P0, "Scroll Thief", 1, 3, SCROLL_THIEF)
        .with_subtypes(vec!["Merfolk", "Rogue"])
        .id();
    let mut runner = s.build();
    let before = hand_size(&runner);

    attack_unblocked(&mut runner, &[thief]);

    assert_eq!(hand_size(&runner) - before, 2);
}

/// CR 603.2d + CR 510.2: the "one or more creatures you control deal combat
/// damage to a player" batch trigger is also caused by a creature you control
/// dealing combat damage, so Professional Face-Breaker's trigger triggers
/// twice: two Treasures. Revert-to-red: a Felix arm that never matches the
/// step's `CombatDamageDealtToPlayer` batch leaves one Treasure.
#[test]
fn felix_five_boots_doubles_a_one_or_more_combat_damage_trigger() {
    let mut s = scenario();
    let felix = add_felix(&mut s, P0);
    s.add_creature(P0, "Professional Face-Breaker", 2, 3)
        .with_subtypes(vec!["Human", "Warrior"])
        .from_oracle_text_with_keywords(&["Menace"], PROFESSIONAL_FACE_BREAKER);
    let mut runner = s.build();

    attack_unblocked(&mut runner, &[felix]);

    let treasures = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].name == "Treasure")
        .count();
    assert_eq!(treasures, 2);
}

/// CR 603.2d: Jabs, Mistress of Mockery doubles triggers of "a permanent you
/// control but don't own". Its scope now parses (the doubler's `And`
/// conjunction is accepted as restrictive), so Witherbloom Apprentice's
/// Magecraft is doubled only while P0 controls it without owning it.
/// Revert-to-red: with no parsed scope, the owned Apprentice is doubled too.
#[test]
fn jabs_doubles_only_triggers_of_permanents_you_control_but_dont_own() {
    for (owner, expected) in [(P0, 1), (P1, 2)] {
        let mut s = scenario();
        s.add_creature_from_oracle(P0, "Jabs, Mistress of Mockery", 1, 1, JABS);
        let apprentice = add_apprentice(&mut s);
        let spell = add_instant(&mut s, "Quick Study", QUICK_STUDY);
        let mut runner = s.build();
        runner
            .state_mut()
            .objects
            .get_mut(&apprentice)
            .unwrap()
            .owner = owner;
        let before = life(&runner);

        cast(&mut runner, spell);
        drive(&mut runner, true);

        assert_eq!(
            life(&runner) - before,
            expected,
            "Apprentice owned by {owner:?}"
        );
    }
}
