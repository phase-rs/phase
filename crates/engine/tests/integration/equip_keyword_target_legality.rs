//! The equip keyword route (CR 702.6a: "Equip [cost]" means "[Cost]: Attach
//! this permanent to target creature you control. Activate only as a
//! sorcery."; CR 115.1e: equip is a targeted activated ability whose "target"
//! appears in the keyword's rule). `GameAction::Equip` at priority announces
//! the equip, `WaitingFor::EquipTarget` takes its one target, and the
//! `StackEntryKind::KeywordAction` entry resolves through
//! `stack::resolve_keyword_action`.
//!
//! The rows pin that the equip's target is a target like any other: offered
//! only when the Equipment's ability can target it (CR 601.2c via CR 602.2b),
//! and rechecked at resolution (CR 608.2b), so an equip whose creature left and
//! returned (CR 400.7), changed controller, phased out, or gained shroud or
//! protection does not resolve. Every step is a production action.

use std::collections::BTreeSet;

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, KeywordAction, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, ShardChoice, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.";
const BONESPLITTER: &str = "Equipped creature gets +2/+0.\nEquip {1}";
const EPHEMERATE: &str = "Exile target creature you control, then return it to the battlefield under its owner's control.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
const RAY_OF_COMMAND: &str = "Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.";
const SLIP_OUT_THE_BACK: &str = "Put a +1/+1 counter on target creature. It phases out. (Treat it and anything attached to it as though they don't exist until its controller's next turn.)";
const VEIL_OF_SECRECY: &str = "Target creature gains shroud until end of turn and can't be blocked this turn. (A creature with shroud can't be the target of spells or abilities.)\nSplice onto Arcane—Return a blue creature you control to its owner's hand. (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)";
const APOSTLES_BLESSING: &str = "({W/P} can be paid with either {W} or 2 life.)\nTarget artifact or creature you control gains protection from artifacts or from the color of your choice until end of turn.";
const LIGHTNING_GREAVES: &str = "Equipped creature has haste and shroud. (It can't be the target of spells or abilities.)\nEquip {0}";

fn mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn spell(
    s: &mut GameScenario,
    player: PlayerId,
    name: &str,
    text: &str,
    mana_cost: ManaCost,
) -> ObjectId {
    s.add_spell_to_hand_from_oracle(player, name, true, text)
        .with_mana_cost(mana_cost)
        .id()
}

fn bonesplitter(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_artifact_from_oracle(player, "Bonesplitter", BONESPLITTER)
        .with_subtypes(vec!["Equipment"])
        .id()
}

fn pass(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("pass priority");
}

fn assert_priority(runner: &GameRunner, player: PlayerId, context: &str) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player),
        "{context}: expected priority for {player:?}, got {:?}",
        runner.state().waiting_for
    );
}

fn stack_ids(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().stack.iter().map(|e| e.id).collect()
}

fn set(ids: &[ObjectId]) -> BTreeSet<ObjectId> {
    ids.iter().copied().collect()
}

/// Announces `equipment`'s equip at priority, answers the target prompt with
/// `target`, and returns the equip's stack entry.
fn equip(runner: &mut GameRunner, equipment: ObjectId, target: ObjectId) -> ObjectId {
    let before = runner.state().stack.len();
    let action = GameAction::Equip {
        equipment_id: equipment,
        target_id: target,
    };
    runner.act(action.clone()).expect("announce equip");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::EquipTarget { .. }),
        "the equip asks for its target: {:?}",
        runner.state().waiting_for
    );
    runner.act(action).expect("choose equip target");
    assert_eq!(
        runner.state().stack.len(),
        before + 1,
        "the equip is on the stack"
    );
    runner.state().stack.back().expect("stack entry").id
}

fn cast(runner: &mut GameRunner, spell: ObjectId, targets: &[ObjectId]) -> ObjectId {
    let before = runner.state().stack.len();
    runner.cast(spell).target_objects(targets).commit();
    assert_eq!(
        runner.state().stack.len(),
        before + 1,
        "the spell is on the stack"
    );
    runner.state().stack.back().expect("stack entry").id
}

/// What happens while the second equip waits on the stack.
#[derive(Clone, Copy, Debug)]
enum EquipChange {
    None,
    Flickered,
    EquipmentFlickered,
    EquipmentPhasedOut,
    ControlTaken,
    PhasedOut,
    Shrouded,
    Protected,
}

/// The board after the second equip left the stack.
struct EquipOutcome {
    runner: GameRunner,
    blade: ObjectId,
    bears: ObjectId,
    vanguard: ObjectId,
    events: Vec<GameEvent>,
}

/// P0 equips Bonesplitter onto Elite Vanguard (it resolves), then equips it
/// onto Grizzly Bears; after the response, both players pass
/// and the second equip leaves the stack. The events are those of the passing
/// action that resolved it.
fn equip_then(change: EquipChange) -> EquipOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let vanguard = scenario.add_creature(P0, "Elite Vanguard", 2, 1).id();
    let blade = bonesplitter(&mut scenario, P0);
    let ephemerate = spell(
        &mut scenario,
        P0,
        "Ephemerate",
        EPHEMERATE,
        cost(&[ManaCostShard::White], 0),
    );
    let flicker = spell(
        &mut scenario,
        P0,
        "Ghostly Flicker",
        GHOSTLY_FLICKER,
        cost(&[ManaCostShard::Blue], 2),
    );
    let slip_out = spell(
        &mut scenario,
        P0,
        "Slip Out the Back",
        SLIP_OUT_THE_BACK,
        cost(&[ManaCostShard::Blue], 0),
    );
    let apostles_blessing = spell(
        &mut scenario,
        P0,
        "Apostle's Blessing",
        APOSTLES_BLESSING,
        cost(&[ManaCostShard::PhyrexianWhite], 1),
    );
    let ray_of_command = spell(
        &mut scenario,
        P1,
        "Ray of Command",
        RAY_OF_COMMAND,
        cost(&[ManaCostShard::Blue], 3),
    );
    let veil_of_secrecy = spell(
        &mut scenario,
        P1,
        "Veil of Secrecy",
        VEIL_OF_SECRECY,
        cost(&[ManaCostShard::Blue], 1),
    );
    scenario.with_mana_pool(
        P0,
        [
            mana(2, ManaType::White),
            mana(1, ManaType::Blue),
            mana(4, ManaType::Colorless),
        ]
        .concat(),
    );
    scenario.with_mana_pool(
        P1,
        [mana(2, ManaType::Blue), mana(4, ManaType::Colorless)].concat(),
    );
    let mut runner = scenario.build();

    equip(&mut runner, blade, vanguard);
    pass(&mut runner);
    pass(&mut runner);
    assert_eq!(
        runner.state().objects[&blade].attached_to,
        Some(AttachTarget::Object(vanguard)),
        "reach guard: the first equip put Bonesplitter on Elite Vanguard"
    );
    assert!(runner.state().stack.is_empty(), "reach guard: stack empty");
    assert_priority(&runner, P0, "after the first equip resolved");

    let equip_entry = equip(&mut runner, blade, bears);
    let incarnation_before = runner.state().objects[&bears].incarnation;
    match change {
        EquipChange::None => {}
        EquipChange::EquipmentFlickered => {
            let original = runner.state().objects[&blade].incarnation;
            cast(&mut runner, flicker, &[blade, vanguard]);
            pass(&mut runner);
            pass(&mut runner);
            let returned = &runner.state().objects[&blade];
            assert_eq!(returned.zone, Zone::Battlefield);
            assert_ne!(returned.incarnation, original);
            assert_eq!(returned.attached_to, None);
        }
        EquipChange::EquipmentPhasedOut => {
            cast(&mut runner, slip_out, &[vanguard]);
            pass(&mut runner);
            pass(&mut runner);
            let blade_now = &runner.state().objects[&blade];
            assert!(
                blade_now.is_phased_out(),
                "Bonesplitter phased out with its host"
            );
            assert_eq!(blade_now.attached_to, Some(AttachTarget::Object(vanguard)));
        }
        EquipChange::Flickered => {
            let eph = cast(&mut runner, ephemerate, &[bears]);
            pass(&mut runner);
            pass(&mut runner);
            assert!(
                !stack_ids(&runner).contains(&eph),
                "reach guard: Ephemerate resolved"
            );
            let bears_now = &runner.state().objects[&bears];
            assert_eq!(
                bears_now.zone,
                Zone::Battlefield,
                "reach guard: Ephemerate returned the Bears"
            );
            assert_ne!(
                bears_now.incarnation, incarnation_before,
                "reach guard: the returned Bears is a new object (CR 400.7)"
            );
        }
        EquipChange::ControlTaken => {
            pass(&mut runner);
            assert_priority(&runner, P1, "P0 passed with the equip on the stack");
            cast(&mut runner, ray_of_command, &[bears]);
            pass(&mut runner);
            pass(&mut runner);
            let bears_now = &runner.state().objects[&bears];
            assert_eq!(
                bears_now.controller, P1,
                "reach guard: Ray of Command took control of the Bears"
            );
            assert_eq!(bears_now.zone, Zone::Battlefield);
            assert_eq!(
                bears_now.incarnation, incarnation_before,
                "reach guard: no zone change, so the pin still names the Bears"
            );
        }
        EquipChange::PhasedOut => {
            cast(&mut runner, slip_out, &[bears]);
            pass(&mut runner);
            pass(&mut runner);
            let bears_now = &runner.state().objects[&bears];
            assert!(
                bears_now.is_phased_out(),
                "reach guard: Slip Out the Back phased the Bears out"
            );
            assert_eq!(bears_now.zone, Zone::Battlefield);
            assert_eq!(
                bears_now.incarnation, incarnation_before,
                "reach guard: phasing is no zone change (CR 702.26d), so the pin \
                 still names the Bears"
            );
        }
        EquipChange::Shrouded => {
            pass(&mut runner);
            assert_priority(&runner, P1, "P0 passed with the equip on the stack");
            cast(&mut runner, veil_of_secrecy, &[bears]);
            pass(&mut runner);
            pass(&mut runner);
            let bears_now = &runner.state().objects[&bears];
            assert!(
                bears_now.has_keyword(&Keyword::Shroud),
                "reach guard: Veil of Secrecy gave the Bears shroud"
            );
            assert_eq!(bears_now.zone, Zone::Battlefield);
            assert_eq!(
                bears_now.controller, P0,
                "reach guard: P0 still controls it"
            );
            assert_eq!(
                bears_now.incarnation, incarnation_before,
                "reach guard: no zone change, so the pin still names the Bears"
            );
        }
        EquipChange::Protected => {
            // The fluent cast driver does not answer `PhyrexianPayment`; the
            // cast goes through the same public actions by hand.
            let card_id = runner.state().objects[&apostles_blessing].card_id;
            runner
                .act(GameAction::CastSpell {
                    object_id: apostles_blessing,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("Apostle's Blessing is announced");
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::TargetSelection { .. }
                ),
                "Apostle's Blessing asks for its target: {:?}",
                runner.state().waiting_for
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(bears)),
                })
                .expect("the Bears is chosen");
            let WaitingFor::PhyrexianPayment { shards, .. } = &runner.state().waiting_for else {
                panic!(
                    "Apostle's Blessing asks how its {{W/P}} is paid: {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(shards.len(), 1);
            runner
                .act(GameAction::SubmitPhyrexianChoices {
                    choices: vec![ShardChoice::PayMana],
                })
                .expect("the {W/P} is paid with white mana");
            assert_priority(&runner, P0, "after Apostle's Blessing is cast");
            pass(&mut runner);
            pass(&mut runner);
            let WaitingFor::ChooseOneOfBranch {
                player,
                branch_descriptions,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "reach guard: Apostle's Blessing asks artifacts or a color: {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(*player, P0, "the caster chooses");
            let artifacts = branch_descriptions
                .iter()
                .position(|d| d.contains("artifacts"))
                .unwrap_or_else(|| panic!("an artifacts branch: {branch_descriptions:?}"));
            runner
                .act(GameAction::ChooseBranch { index: artifacts })
                .expect("protection from artifacts is chosen");
            let bears_now = &runner.state().objects[&bears];
            assert!(
                bears_now.has_keyword(&Keyword::Protection(ProtectionTarget::CardType(
                    "artifacts".to_string()
                ))),
                "reach guard: the Bears has protection from artifacts: {:?}",
                bears_now.keywords
            );
            assert_eq!(bears_now.zone, Zone::Battlefield);
            assert_eq!(
                bears_now.controller, P0,
                "reach guard: P0 still controls it"
            );
            assert_eq!(
                bears_now.incarnation, incarnation_before,
                "reach guard: no zone change, so the pin still names the Bears"
            );
        }
    }
    assert_eq!(
        stack_ids(&runner),
        vec![equip_entry],
        "reach guard: only the second equip waits"
    );
    assert_priority(&runner, P0, "the second equip waits for both passes");
    pass(&mut runner);
    let events = runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority")
        .events;
    assert!(
        events.iter().any(
            |e| matches!(e, GameEvent::StackResolved { object_id } if *object_id == equip_entry)
        ),
        "reach guard: the second equip left the stack: {events:?}"
    );
    assert!(stack_ids(&runner).is_empty(), "the stack is empty");
    EquipOutcome {
        runner,
        blade,
        bears,
        vanguard,
        events,
    }
}

fn equip_resolved(outcome: &EquipOutcome) -> bool {
    outcome.events.iter().any(|e| {
        matches!(
            e,
            GameEvent::EffectResolved { kind: EffectKind::Equip, source_id, .. }
                if *source_id == outcome.blade
        )
    })
}

/// Asserts that the second equip did not resolve: Bonesplitter stays on Elite
/// Vanguard and no `EffectResolved` names it (CR 608.2b; Dissection Tools
/// ruling: "If you sacrifice the creature targeted by the equip ability, then
/// the ability won't resolve and the Equipment will remain attached to
/// whatever it was attached to (if anything).").
fn assert_equip_did_not_resolve(outcome: &EquipOutcome, context: &str) {
    let state = outcome.runner.state();
    assert_eq!(
        state.objects[&outcome.blade].attached_to,
        Some(AttachTarget::Object(outcome.vanguard)),
        "{context}: Bonesplitter stays on Elite Vanguard"
    );
    assert!(
        !state.objects[&outcome.bears]
            .attachments
            .contains(&outcome.blade),
        "{context}: the Bears did not get Bonesplitter"
    );
    assert!(
        !equip_resolved(outcome),
        "{context}: an equip that doesn't resolve emits no EffectResolved: {:?}",
        outcome.events
    );
}

/// CR 608.2b: after the source leaves, Black Knight's protection checks the
/// color Bonesplitter had at exit, including a Purelace resolved after activation.
#[test]
fn equip_source_last_known_color_rechecks_protection() {
    use engine::types::mana::ManaColor;

    for whiten in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let knight = scenario.add_creature(P0, "Black Knight", 2, 2)
            .from_oracle_text_with_keywords(
                &["first strike", "protection"],
                "First strike (This creature deals combat damage before creatures without first strike.)\nProtection from white (This creature can't be blocked, targeted, dealt damage, or enchanted by anything white.)",
            )
            .id();
        let vanguard = scenario.add_creature(P0, "Elite Vanguard", 2, 1).id();
        let blade = bonesplitter(&mut scenario, P0);
        let lace = spell(
            &mut scenario, P0, "Purelace",
            "Target spell or permanent becomes white. (Mana symbols on that permanent remain unchanged.)",
            cost(&[ManaCostShard::White], 0),
        );
        let flicker = spell(
            &mut scenario,
            P0,
            "Ghostly Flicker",
            GHOSTLY_FLICKER,
            cost(&[ManaCostShard::Blue], 2),
        );
        scenario.with_mana_pool(
            P0,
            [
                mana(1, ManaType::White),
                mana(1, ManaType::Blue),
                mana(4, ManaType::Colorless),
            ]
            .concat(),
        );
        let mut runner = scenario.build();
        equip(&mut runner, blade, vanguard);
        pass(&mut runner);
        pass(&mut runner);
        assert_eq!(
            runner.state().objects[&blade].attached_to,
            Some(AttachTarget::Object(vanguard))
        );
        assert!(runner.state().objects[&knight]
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::White
            ))));
        assert!(runner.state().objects[&blade].effective_colors().is_empty());
        let equip_entry = equip(&mut runner, blade, knight);
        let original = runner.state().objects[&blade].incarnation;
        if whiten {
            cast(&mut runner, lace, &[blade]);
            pass(&mut runner);
            pass(&mut runner);
            assert_eq!(
                runner.state().objects[&blade].effective_colors(),
                vec![ManaColor::White]
            );
        }
        cast(&mut runner, flicker, &[blade, vanguard]);
        pass(&mut runner);
        pass(&mut runner);
        let returned = &runner.state().objects[&blade];
        assert_eq!(returned.zone, Zone::Battlefield);
        assert_ne!(returned.incarnation, original);
        assert!(returned.effective_colors().is_empty());
        assert_eq!(stack_ids(&runner), vec![equip_entry]);
        pass(&mut runner);
        let events = runner
            .act(GameAction::PassPriority)
            .expect("resolve equip")
            .events;
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::StackResolved { object_id } if *object_id == equip_entry)
        ));
        let resolved = events.iter().any(|e| matches!(e, GameEvent::EffectResolved { kind: EffectKind::Equip, source_id, .. } if *source_id == blade));
        assert_eq!(
            resolved, !whiten,
            "protection checks the departed Equipment's last color"
        );
        assert_eq!(runner.state().objects[&blade].attached_to, None);
        assert!(!runner.state().objects[&knight].attachments.contains(&blade));
    }
}

/// CR 400.7 and CR 113.7a: the target remains legal, but the returning
/// Equipment is a new permanent. The no-response leg can move the original.
#[test]
fn equip_source_returned_as_a_new_permanent() {
    let outcome = equip_then(EquipChange::EquipmentFlickered);
    assert!(equip_resolved(&outcome));
    assert_eq!(
        outcome.runner.state().objects[&outcome.blade].attached_to,
        None
    );
}

/// CR 702.26b: an Equipment that phased out with its old host cannot move
/// while phased out, even though the target of its ability remains legal.
#[test]
fn equip_source_phased_out_with_its_host() {
    let mut outcome = equip_then(EquipChange::EquipmentPhasedOut);
    assert!(equip_resolved(&outcome));
    assert_eq!(
        outcome.runner.state().objects[&outcome.blade].attached_to,
        Some(AttachTarget::Object(outcome.vanguard))
    );
    assert_priority(
        &outcome.runner,
        P0,
        "before activating the phased-out Equipment",
    );
    assert!(
        outcome
            .runner
            .act(equip_pick(outcome.blade, outcome.bears))
            .is_err(),
        "phased-out Bonesplitter cannot announce another equip"
    );
    assert!(outcome.runner.state().stack.is_empty());
}

/// CR 608.2b + CR 400.7 + CR 702.6a (Dissection Tools ruling): an equip whose
/// creature Ephemerate ("Exile target creature you control, then return it to
/// the battlefield under its owner's control.") flickered does not resolve:
/// the returned creature is a new object, so the equip's one target is illegal
/// and Bonesplitter ("Equipped creature gets +2/+0.\nEquip {1}") stays where it
/// was. Without the flicker the equip moves Bonesplitter to the Bears.
#[test]
fn equip_does_not_attach_to_a_creature_that_was_flickered() {
    let outcome = equip_then(EquipChange::None);
    let state = outcome.runner.state();
    assert_eq!(
        state.objects[&outcome.blade].attached_to,
        Some(AttachTarget::Object(outcome.bears)),
        "the equip moved Bonesplitter to the Bears"
    );
    assert!(
        state.objects[&outcome.bears]
            .attachments
            .contains(&outcome.blade),
        "the Bears carries Bonesplitter"
    );
    assert!(equip_resolved(&outcome), "the equip resolved");

    let outcome = equip_then(EquipChange::Flickered);
    assert_equip_did_not_resolve(&outcome, "Ephemerate flickered the Bears");
}

/// CR 608.2b ("Other changes to the game state may cause a target to no longer
/// be legal") + CR 702.6a ("Attach this permanent to target creature you
/// control."); Blackblade Reforged ruling: "Whether the target creature is
/// legendary is checked only as Blackblade Reforged's first equip ability is
/// activated and as that ability resolves." P1's Ray of Command ("Untap target
/// creature an opponent controls and gain control of it until end of turn.
/// …") takes the Bears, so the equip's target is no longer a creature its
/// controller controls and the equip does not resolve.
#[test]
fn equip_does_not_attach_to_a_creature_an_opponent_took() {
    let outcome = equip_then(EquipChange::ControlTaken);
    assert_equip_did_not_resolve(&outcome, "Ray of Command took the Bears");
}

/// CR 608.2b + CR 702.26b ("a phased-out permanent is treated as though it does
/// not exist"); Robe of Stars ruling: "Phased out permanents are treated as
/// though they don't exist. They can't be the target of spells or abilities,
/// …". Slip Out the Back ("Put a +1/+1 counter on target creature. It phases
/// out. …") phases the Bears out, so the equip's target is illegal and the
/// equip does not resolve.
#[test]
fn equip_does_not_attach_to_a_creature_that_phased_out() {
    let outcome = equip_then(EquipChange::PhasedOut);
    assert_equip_did_not_resolve(&outcome, "Slip Out the Back phased the Bears out");
}

/// CR 608.2b + CR 702.18a ("Shroud" means "This permanent or player can't be
/// the target of spells or abilities."): P1's Veil of Secrecy ("Target
/// creature gains shroud until end of turn and can't be blocked this turn.
/// …") gives the Bears shroud in response, so the equip's one target is
/// illegal and the equip does not resolve. Positive pair: the no-change leg of
/// `equip_does_not_attach_to_a_creature_that_was_flickered`.
#[test]
fn equip_does_not_attach_to_a_creature_that_gained_shroud() {
    let outcome = equip_then(EquipChange::Shrouded);
    assert_equip_did_not_resolve(&outcome, "Veil of Secrecy gave the Bears shroud");
}

/// CR 608.2b + CR 702.16b ("A permanent or player with protection can't be
/// targeted … by abilities from a source with the stated quality."): P0's
/// Apostle's Blessing ("Target artifact or creature you control gains
/// protection from artifacts or from the color of your choice until end of
/// turn."), choosing artifacts, makes the Bears an illegal target for
/// Bonesplitter's equip, so the equip does not resolve and emits no
/// `EffectResolved`. Positive pair: the no-change leg of
/// `equip_does_not_attach_to_a_creature_that_was_flickered`.
#[test]
fn equip_does_not_attach_to_a_creature_that_gained_protection_from_artifacts() {
    let outcome = equip_then(EquipChange::Protected);
    assert_equip_did_not_resolve(
        &outcome,
        "Apostle's Blessing gave the Bears protection from artifacts",
    );
}

/// P0 controls Grizzly Bears, Elite Vanguard, Bonesplitter and Lightning
/// Greaves; if `greaves_on_bears`, P0 first equips the Greaves onto the Bears
/// (it resolves). Then P0 announces Bonesplitter's equip. Returns the runner,
/// the Bears, Elite Vanguard and Bonesplitter.
fn bonesplitter_equip_offer(greaves_on_bears: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let vanguard = scenario.add_creature(P0, "Elite Vanguard", 2, 1).id();
    let blade = bonesplitter(&mut scenario, P0);
    let greaves = scenario
        .add_artifact_from_oracle(P0, "Lightning Greaves", LIGHTNING_GREAVES)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    if greaves_on_bears {
        equip(&mut runner, greaves, bears);
        pass(&mut runner);
        pass(&mut runner);
        assert_eq!(
            runner.state().objects[&greaves].attached_to,
            Some(AttachTarget::Object(bears)),
            "reach guard: Lightning Greaves is on the Bears"
        );
        assert!(
            runner.state().objects[&bears].has_keyword(&Keyword::Shroud),
            "reach guard: the Greaves give the Bears shroud"
        );
    }
    assert!(runner.state().stack.is_empty(), "reach guard: stack empty");
    assert_priority(&runner, P0, "before Bonesplitter's equip");
    runner
        .act(GameAction::Equip {
            equipment_id: blade,
            target_id: blade,
        })
        .expect("Bonesplitter's equip is announced");
    (runner, bears, vanguard, blade)
}

/// CR 702.6a + CR 601.2c + CR 702.18a: equip's announcement offers only
/// creatures its ability can target. With Lightning Greaves ("Equipped
/// creature has haste and shroud. (It can't be the target of spells or
/// abilities.)\nEquip {0}") on the Bears, Elite Vanguard is the one legal
/// target, so Bonesplitter's equip goes on the stack targeting it without a
/// prompt (Lightning Greaves ruling: "If Lightning Greaves is attached to the
/// only creature you control, you won't be able to attach other equipment to
/// it (or target it with anything else) …"). Without the Greaves on the Bears,
/// both creatures are offered.
#[test]
fn equip_does_not_offer_a_creature_with_shroud() {
    let (runner, bears, vanguard, _) = bonesplitter_equip_offer(false);
    let WaitingFor::EquipTarget { valid_targets, .. } = &runner.state().waiting_for else {
        panic!(
            "two legal creatures, so the equip asks for its target: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        valid_targets.iter().copied().collect::<BTreeSet<_>>(),
        set(&[bears, vanguard]),
        "without shroud both creatures are offered"
    );

    let (runner, bears, vanguard, blade) = bonesplitter_equip_offer(true);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EquipTarget { ref valid_targets, .. } if valid_targets.contains(&bears)
        ),
        "CR 702.18a: the shrouded Bears is not offered: {:?}",
        runner.state().waiting_for
    );
    assert_priority(
        &runner,
        P0,
        "the one legal target is chosen without a prompt",
    );
    let entry = runner
        .state()
        .stack
        .back()
        .expect("the equip is on the stack");
    let StackEntryKind::KeywordAction { action } = &entry.kind else {
        panic!("the top of the stack is the equip: {:?}", entry.kind);
    };
    let KeywordAction::Equip { equipment, .. } = action else {
        panic!("the top of the stack is an equip: {action:?}");
    };
    assert_eq!(equipment.object_id, blade);
    assert_eq!(
        equipment.incarnation,
        runner.state().objects[&blade].incarnation
    );
    assert_eq!(
        action.targets(),
        vec![TargetRef::Object(vanguard)],
        "Elite Vanguard is the equip's only legal target"
    );
}

fn equip_pick(equipment_id: ObjectId, target_id: ObjectId) -> GameAction {
    GameAction::Equip {
        equipment_id,
        target_id,
    }
}

fn assert_equip_offer(wait: &WaitingFor, blade: ObjectId, expected: &[ObjectId]) {
    let WaitingFor::EquipTarget {
        player,
        equipment_id,
        valid_targets,
    } = wait
    else {
        panic!("expected an equip target choice, got {wait:?}");
    };
    assert_eq!((*player, *equipment_id), (P0, blade));
    assert_eq!(set(valid_targets), set(expected));
}

/// Older saves kept the original offer alongside the post-concession board.
fn save_with_original_offer(runner: &GameRunner, original: &WaitingFor) -> serde_json::Value {
    let mut saved = serde_json::to_value(runner.state()).expect("save pending game");
    saved["waiting_for"] = serde_json::to_value(original).expect("save original offer");
    saved
}

const HUMILITY: &str = "All creatures lose all abilities and have base power and toughness 1/1.";
const NIMBLE_MONGOOSE: &str = "Shroud (This creature can't be the target of spells or abilities.)\nThreshold — This creature gets +2/+2 as long as there are seven or more cards in your graveyard.";

#[test]
fn equip_pending_target_restores_shroud_when_humilitys_owner_concedes() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let blade = bonesplitter(&mut scenario, P0);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mongoose = scenario
        .add_creature(P0, "Nimble Mongoose", 1, 1)
        .from_oracle_text_with_keywords(&["Shroud"], NIMBLE_MONGOOSE)
        .id();
    scenario.add_enchantment_from_oracle(p2, "Humility", HUMILITY);
    let mut runner = scenario.build();
    assert!(runner.state().objects[&mongoose]
        .base_keywords
        .contains(&Keyword::Shroud));
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert!(!runner.state().objects[&mongoose].has_keyword(&Keyword::Shroud));
    let original = runner
        .act(equip_pick(blade, blade))
        .expect("announce equip");
    assert_equip_offer(&original.waiting_for, blade, &[mongoose, bears]);
    let departure = runner
        .act(GameAction::Concede { player_id: p2 })
        .expect("P2 concedes");
    assert!(runner.state().objects[&mongoose].has_keyword(&Keyword::Shroud));

    let saved = save_with_original_offer(&runner, &original.waiting_for);
    let mut loaded =
        GameRunner::from_state(serde_json::from_value(saved.clone()).expect("load pending game"));
    assert_equip_offer(&loaded.state().waiting_for, blade, &[mongoose, bears]);
    assert!(
        loaded.act(equip_pick(blade, mongoose)).is_err(),
        "a stored offer cannot authorize a shrouded target"
    );
    assert!(loaded.state().stack.is_empty());
    loaded
        .act(equip_pick(blade, bears))
        .expect("the Bears remain legal after loading");

    let restored = serde_json::from_value::<engine::types::game_state::PersistedGameState>(saved)
        .expect("decode legacy save")
        .into_game_state()
        .expect("finalize persisted restore");
    assert_equip_offer(&restored.waiting_for, blade, &[bears]);

    assert_equip_offer(&departure.waiting_for, blade, &[bears]);
    assert!(runner.act(equip_pick(blade, mongoose)).is_err());
    assert!(runner.state().stack.is_empty());
    runner
        .act(equip_pick(blade, bears))
        .expect("choose the Bears");
    pass(&mut runner);
    pass(&mut runner);
    assert_eq!(
        runner.state().objects[&blade].attached_to,
        Some(AttachTarget::Object(bears))
    );
}

#[test]
fn equip_pending_target_offers_a_creature_released_from_lightning_greaves() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let blade = bonesplitter(&mut scenario, P0);
    let greaves = scenario
        .add_artifact_from_oracle(p2, "Lightning Greaves", LIGHTNING_GREAVES)
        .with_subtypes(vec!["Equipment"])
        .controlled_by(P0)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let vanguard = scenario.add_creature(P0, "Elite Vanguard", 2, 1).id();
    let lions = scenario.add_creature(P0, "Savannah Lions", 2, 1).id();
    let mut runner = scenario.build();
    equip(&mut runner, greaves, bears);
    for _ in 0..3 {
        pass(&mut runner);
    }
    assert_eq!(
        runner.state().objects[&greaves].attached_to,
        Some(AttachTarget::Object(bears))
    );
    assert!(runner.state().objects[&bears].has_keyword(&Keyword::Shroud));
    let original = runner
        .act(equip_pick(blade, blade))
        .expect("announce equip");
    assert_equip_offer(&original.waiting_for, blade, &[vanguard, lions]);

    let departure = runner
        .act(GameAction::Concede { player_id: p2 })
        .expect("P2 takes the Greaves out of the game");
    assert!(!runner.state().objects[&bears].has_keyword(&Keyword::Shroud));
    let saved = save_with_original_offer(&runner, &original.waiting_for);
    let mut loaded =
        GameRunner::from_state(serde_json::from_value(saved).expect("load pending game"));
    assert_equip_offer(&loaded.state().waiting_for, blade, &[vanguard, lions]);
    loaded
        .act(equip_pick(blade, bears))
        .expect("a stale omission cannot bar a legal pick");
    assert_equip_offer(&departure.waiting_for, blade, &[bears, vanguard, lions]);
    runner
        .act(equip_pick(blade, bears))
        .expect("choose the newly legal Bears");
    pass(&mut runner);
    pass(&mut runner);
    assert_eq!(
        runner.state().objects[&blade].attached_to,
        Some(AttachTarget::Object(bears))
    );
}

#[test]
fn equip_pending_target_can_cancel_after_the_equipment_leaves_with_its_owner() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let blade = scenario
        .add_artifact_from_oracle(p2, "Bonesplitter", BONESPLITTER)
        .with_subtypes(vec!["Equipment"])
        .controlled_by(P0)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let vanguard = scenario.add_creature(P0, "Elite Vanguard", 2, 1).id();
    let mut runner = scenario.build();
    let original = runner
        .act(equip_pick(blade, blade))
        .expect("announce equip");
    assert_equip_offer(&original.waiting_for, blade, &[bears, vanguard]);
    let departure = runner
        .act(GameAction::Concede { player_id: p2 })
        .expect("P2 takes Bonesplitter out of the game");
    assert_ne!(runner.state().objects[&blade].zone, Zone::Battlefield);
    assert!(
        runner.act(equip_pick(blade, bears)).is_err(),
        "the departed Equipment cannot announce an equip"
    );
    assert!(runner.state().stack.is_empty());
    assert_equip_offer(&departure.waiting_for, blade, &[]);
    runner
        .act(GameAction::CancelCast)
        .expect("back out of equip");
    assert_priority(&runner, P0, "after abandoning the empty offer");
}

/// CR 113.8 and CR 702.16k read different controllers for targeting and attaching
/// after Memnarch steals the Equipment in response to its equip ability.
fn equip_after_memnarch_takes_bonesplitter(chosen: PlayerId) {
    const TRUE_NAME: &str = "As this creature enters, choose a player.\nThis creature has protection from the chosen player.";
    const MEMNARCH: &str = "{1}{U}{U}: Target permanent becomes an artifact in addition to its other types. (This effect lasts indefinitely.)\n{3}{U}: Gain control of target artifact. (This effect lasts indefinitely.)";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blade = bonesplitter(&mut scenario, P0);
    let old_host = scenario.add_creature(P0, "Elite Vanguard", 2, 1).id();
    let nemesis = scenario
        .add_creature_to_hand_from_oracle(P0, "True-Name Nemesis", 3, 1, TRUE_NAME)
        .with_mana_cost(cost(&[ManaCostShard::Blue, ManaCostShard::Blue], 1))
        .id();
    let memnarch = scenario
        .add_creature(P1, "Memnarch", 4, 5)
        .as_artifact_creature()
        .as_legendary()
        .from_oracle_text(MEMNARCH)
        .id();
    let humility = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Humility", HUMILITY)
        .with_mana_cost(cost(&[ManaCostShard::White, ManaCostShard::White], 2))
        .id();
    let disenchant = spell(
        &mut scenario,
        P0,
        "Disenchant",
        "Destroy target artifact or enchantment.",
        cost(&[ManaCostShard::White], 1),
    );
    scenario.with_mana_pool(
        P0,
        [
            mana(3, ManaType::White),
            mana(2, ManaType::Blue),
            mana(6, ManaType::Colorless),
        ]
        .concat(),
    );
    scenario.with_mana_pool(P1, mana(4, ManaType::Blue));
    let mut runner = scenario.build();
    cast(&mut runner, nemesis, &[]);
    runner.resolve_top();
    runner
        .act(GameAction::ChooseOption {
            choice: chosen.0.to_string(),
        })
        .expect("choose a player as True-Name Nemesis enters");
    let protection = Keyword::Protection(ProtectionTarget::ChosenPlayer);
    assert_eq!(
        runner.state().objects[&nemesis].chosen_player(),
        Some(chosen)
    );
    assert!(runner.state().objects[&nemesis].has_keyword(&protection));
    // Suppress protection for announcement only when the activator was chosen.
    if chosen == P0 {
        cast(&mut runner, humility, &[]);
        runner.resolve_top();
        assert!(!runner.state().objects[&nemesis].has_keyword(&protection));
    }
    equip(&mut runner, blade, old_host);
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&blade].attached_to,
        Some(AttachTarget::Object(old_host))
    );

    let equip_entry = equip(&mut runner, blade, nemesis);
    if chosen == P0 {
        cast(&mut runner, disenchant, &[humility]);
        runner.resolve_top();
        assert_eq!(runner.state().objects[&humility].zone, Zone::Graveyard);
    }
    assert!(runner.state().objects[&nemesis].has_keyword(&protection));
    pass(&mut runner);
    assert_priority(&runner, P1, "Memnarch can respond to equip");
    runner
        .act(GameAction::ActivateAbility {
            source_id: memnarch,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Memnarch to gain control");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(blade)),
        })
        .expect("target Bonesplitter");
    runner.resolve_top();
    assert_eq!(runner.state().objects[&blade].controller, P1);
    assert_eq!(stack_ids(&runner), vec![equip_entry]);
    assert_eq!(runner.state().stack.back().unwrap().controller, P0);

    pass(&mut runner);
    let events = runner
        .act(GameAction::PassPriority)
        .expect("resolve the remaining stack entry")
        .events;
    assert!(runner.state().stack.is_empty());
    assert!(events.iter().any(
        |event| matches!(event, GameEvent::StackResolved { object_id } if *object_id == equip_entry)
    ));
    let resolved = events.iter().any(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::Equip, source_id, .. } if *source_id == blade));
    assert_eq!(
        resolved,
        chosen != P0,
        "protection checks the controller of the equip ability"
    );
    assert_eq!(
        runner.state().objects[&blade].attached_to,
        Some(AttachTarget::Object(old_host)),
        "the stolen Equipment stays on its old host"
    );
    assert!(!runner.state().objects[&nemesis]
        .attachments
        .contains(&blade));
}

#[test]
fn equip_resolves_without_attaching_when_the_chosen_player_steals_the_equipment() {
    equip_after_memnarch_takes_bonesplitter(P1);
}

#[test]
fn equip_does_not_resolve_when_protection_from_its_activator_returns_after_announcement() {
    equip_after_memnarch_takes_bonesplitter(P0);
}
