//! CR 603.2d + CR 707.10: Veyran, Voice of Duality doubles its own Magecraft.
//!
//! Oracle: "Magecraft — Whenever you cast or copy an instant or sorcery spell,
//! Veyran gets +1/+1 until end of turn. If you casting or copying an instant or
//! sorcery spell causes a triggered ability of a permanent you control to
//! trigger, that ability triggers an additional time."
//!
//! Veyran is "a permanent you control", so its own Magecraft is in the
//! doubler's scope and triggers twice per instant or sorcery its controller
//! casts or copies (https://github.com/phase-rs/phase/issues/8838). A doubler
//! that says "another" (Harmonic Prodigy) still leaves its own triggers alone,
//! because its scope excludes itself.
//!
//! Signals: Veyran's power counts how many of its Magecraft instances resolved
//! (+1 each); an observer's "you gain 1 life" Magecraft counts its own.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const VEYRAN: &str = "Magecraft — Whenever you cast or copy an instant or sorcery spell, Veyran gets +1/+1 until end of turn.\nIf you casting or copying an instant or sorcery spell causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";
const MAGECRAFT_OBSERVER: &str =
    "Magecraft — Whenever you cast or copy an instant or sorcery spell, you gain 1 life.";
const HARMONIC_PRODIGY: &str = "Prowess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)\nIf a triggered ability of a Shaman or another Wizard you control triggers, that ability triggers an additional time.";
const DRAW: &str = "Draw a card.";
const TARGET_DRAW: &str = "Target player draws a card.";
/// Copies without a new-targets choice (the copy resolver's immediate drain).
const COPY: &str = "Copy target instant or sorcery spell.";
/// Twincast: copies with a CR 707.10c new-targets choice when the copied spell
/// has targets (the copy resolver's deferred drain after the retarget walk).
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";

fn scenario() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for i in 0..16 {
        s.add_card_to_library_top(P0, &format!("P0 Lib {i}"));
    }
    s
}

fn add_veyran(s: &mut GameScenario) -> ObjectId {
    s.add_creature_from_oracle(P0, "Veyran, Voice of Duality", 2, 2, VEYRAN)
        .with_subtypes(vec!["Efreet", "Wizard"])
        .id()
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

/// Answer prompts with their first legal choice; resolve the stack fully when
/// `resolve` is set, else stop at the first priority window. Returns the
/// events of every action taken.
fn drive(runner: &mut GameRunner, resolve: bool) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
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
                let target = target_slots[selection.current_slot]
                    .legal_targets
                    .first()
                    .cloned();
                events.extend(
                    runner
                        .act(GameAction::ChooseTarget { target })
                        .expect("choose target")
                        .events,
                );
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::CopyRetarget { .. } => {
                events.extend(
                    runner
                        .act(GameAction::KeepAllCopyTargets)
                        .expect("keep the copy's targets")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => {
                if !resolve || runner.state().stack.is_empty() {
                    return events;
                }
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority")
                        .events,
                );
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("drive did not settle");
}

/// Cast `first`, respond with the copy spell `copier` (it targets `first`, the
/// only spell on the stack), and resolve everything. Returns how many
/// `SpellCopied` events the copy produced.
fn cast_then_copy(runner: &mut GameRunner, first: ObjectId, copier: ObjectId) -> usize {
    cast(runner, first);
    drive(runner, false);
    let mut events = cast(runner, copier);
    events.extend(drive(runner, true));
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::SpellCopied { .. }))
        .count()
}

/// Casting an instant gives exactly two instances of Veyran's own Magecraft.
/// Revert-to-red: restoring the blanket self-exclusion in `doubler_applies`
/// leaves one instance (power 3).
#[test]
fn veyran_doubles_its_own_magecraft_on_cast() {
    let mut s = scenario();
    let veyran = add_veyran(&mut s);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Quick Study", true, DRAW)
        .id();
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
/// `SpellCopied`, on the copy resolver's immediate path. Casts: the drawn
/// spell (+2) and the copy spell (+2); the copy (+2). Revert-to-red: the
/// self-exclusion leaves +1 per event (power 5); a second collection of the
/// same `SpellCopied` adds extra instances (power above 8).
#[test]
fn veyran_doubles_its_own_magecraft_on_copy() {
    let mut s = scenario();
    let veyran = add_veyran(&mut s);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Quick Study", true, DRAW)
        .id();
    let copier = s
        .add_spell_to_hand_from_oracle(P0, "Echo Spell", true, COPY)
        .id();
    let mut runner = s.build();

    let copies = cast_then_copy(&mut runner, spell, copier);

    assert_eq!(copies, 1, "one copy, one SpellCopied");
    assert_eq!(
        power(&runner, veyran),
        8,
        "2 + 2 (cast) + 2 (cast) + 2 (copy)"
    );
}

/// Same as above through the CR 707.10c retarget walk (Twincast copying a
/// targeted spell), whose copy observers drain after the walk completes.
#[test]
fn veyran_doubles_its_own_magecraft_on_retargetable_copy() {
    let mut s = scenario();
    let veyran = add_veyran(&mut s);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Targeted Study", true, TARGET_DRAW)
        .id();
    let copier = s
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .id();
    let mut runner = s.build();

    let copies = cast_then_copy(&mut runner, spell, copier);

    assert_eq!(copies, 1, "one copy, one SpellCopied");
    assert_eq!(
        power(&runner, veyran),
        8,
        "2 + 2 (cast) + 2 (cast) + 2 (copy)"
    );
}

/// Without a doubler, one copy is one Magecraft trigger: a `SpellCopied` is
/// collected once. Three events (two casts, one copy) gain 3 life.
#[test]
fn copied_spell_triggers_magecraft_once_without_a_doubler() {
    for copy_text in [COPY, TWINCAST] {
        let mut s = scenario();
        s.add_creature_from_oracle(P0, "Apprentice", 1, 1, MAGECRAFT_OBSERVER);
        let spell = s
            .add_spell_to_hand_from_oracle(P0, "Targeted Study", true, TARGET_DRAW)
            .id();
        let copier = s
            .add_spell_to_hand_from_oracle(P0, "Copier", true, copy_text)
            .id();
        let mut runner = s.build();
        let before = life(&runner);

        cast_then_copy(&mut runner, spell, copier);

        assert_eq!(
            life(&runner) - before,
            3,
            "{copy_text}: two casts + one copy"
        );
    }
}

/// Another Magecraft permanent you control is doubled on both cast and copy.
#[test]
fn veyran_doubles_another_magecraft_permanent_on_cast_and_copy() {
    for copy_text in [COPY, TWINCAST] {
        let mut s = scenario();
        add_veyran(&mut s);
        s.add_creature_from_oracle(P0, "Apprentice", 1, 1, MAGECRAFT_OBSERVER);
        let spell = s
            .add_spell_to_hand_from_oracle(P0, "Targeted Study", true, TARGET_DRAW)
            .id();
        let copier = s
            .add_spell_to_hand_from_oracle(P0, "Copier", true, copy_text)
            .id();
        let mut runner = s.build();
        let before = life(&runner);

        cast_then_copy(&mut runner, spell, copier);

        assert_eq!(
            life(&runner) - before,
            6,
            "{copy_text}: three events, each doubled"
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
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Quick Study", true, DRAW)
        .id();
    let copier = s
        .add_spell_to_hand_from_oracle(P0, "Echo Spell", true, COPY)
        .id();
    let mut runner = s.build();

    cast_then_copy(&mut runner, spell, copier);

    // Three events (two casts, one copy), three instances each.
    assert_eq!(power(&runner, first), 2 + 9);
    assert_eq!(power(&runner, second), 2 + 9);
}

/// A doubler that says "another" skips its own triggers through its scope:
/// Harmonic Prodigy (a Wizard, not a Shaman) doubles another Wizard's prowess
/// but not its own.
#[test]
fn another_scoped_doubler_does_not_double_itself() {
    let mut s = scenario();
    let prodigy = s
        .add_creature(P0, "Harmonic Prodigy", 1, 3)
        .with_subtypes(vec!["Human", "Wizard"])
        .from_oracle_text_with_keywords(&["Prowess"], HARMONIC_PRODIGY)
        .id();
    let wizard = s
        .add_creature(P0, "Prowess Wizard", 1, 1)
        .with_subtypes(vec!["Human", "Wizard"])
        .from_oracle_text_with_keywords(&["Prowess"], "Prowess")
        .id();
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Quick Study", true, DRAW)
        .id();
    let mut runner = s.build();

    cast(&mut runner, spell);
    drive(&mut runner, true);

    assert_eq!(
        power(&runner, prodigy),
        2,
        "Prodigy's own prowess triggers once"
    );
    assert_eq!(
        power(&runner, wizard),
        3,
        "another Wizard's prowess is doubled"
    );
}
