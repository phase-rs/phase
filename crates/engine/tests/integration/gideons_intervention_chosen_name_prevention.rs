//! Gideon's Intervention — runtime witness for the whole printed card:
//! "As this enchantment enters, choose a card name.
//! Your opponents can't cast spells with the chosen name.
//! Prevent all damage that would be dealt to you and permanents you control by
//! sources with the chosen name."
//!
//! CR 614.1c: "As [this permanent] enters ... choose" is a replacement effect.
//! CR 201.2 + CR 201.4: a chosen card name is public and is checked by name.
//! CR 101.2: a "can't" restriction takes precedence over permission.
//! CR 604.1: the static abilities are simply true while the enchantment is on
//!   the battlefield.
//! CR 609.7c + CR 615.1a + CR 615.2: the prevention effect is scoped to damage
//!   from sources with the chosen name, rechecked when the damage would be dealt.
//! CR 120.3a: damage dealt to a player causes that player to lose that much life.
//!
//! Also the fused-split-spell and Disruptor Flute witnesses of the same
//! chosen-name fix: CR 709.4a + CR 709.4d ("an object has the chosen name if
//! one of its names is the chosen name"; a fused split spell has both halves'
//! characteristics) and CR 702.102b + CR 702.102c.
//!
//! Verbatim printed Oracle text (MTGJSON AtomicCards), as above; Toil's
//! "Target player draws two cards and loses 2 life." and Trouble's "Trouble
//! deals damage to target player equal to the number of cards in that player's
//! hand." are read from the committed integration fixture.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChoiceType, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, CastingVariantChoiceOption, CastingVariantFace,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// The printed creature named by every test in this file.
const NAMED_CARD: &str = "Grizzly Bears";

fn db() -> &'static CardDatabase {
    load_db().expect("the committed integration card fixture is present")
}

fn mana(kind: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, Vec::new()))
        .collect()
}

/// Pass priority (or drain a trigger-ordering prompt) until `stop` holds.
fn drive_to(runner: &mut GameRunner, stop: impl Fn(&GameRunner) -> bool) {
    for _ in 0..64 {
        if stop(runner) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers");
            }
            other => panic!("unexpected prompt while advancing: {other:?}"),
        }
    }
    panic!("the game never reached the expected step");
}

fn advance_to_declare_attackers(runner: &mut GameRunner) {
    drive_to(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
}

fn advance_to_declare_blockers(runner: &mut GameRunner) {
    drive_to(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
    });
}

/// Make P1 the active player at its precombat main, so its creatures can be
/// declared as attackers (CR 508.1).
fn begin_p1_attack_step(runner: &mut GameRunner) {
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
}

/// CR 614.1c + CR 201.2: cast an as-enters card-name permanent from P0's hand,
/// assert the as-enters card-name prompt (actor and legal-name domain), name
/// `named`, and return once the permanent is on the battlefield.
fn cast_naming(runner: &mut GameRunner, permanent: ObjectId, named: &str) {
    let card_id = runner.state().objects[&permanent].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: permanent,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|error| panic!("the as-enters permanent must be castable: {error:?}"));
    runner.advance_until_stack_empty();

    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options: prompt_options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the as-enters card-name choice must be offered, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "the permanent's controller names the card");
    let ChoiceType::CardName {
        options: domain, ..
    } = &choice_type
    else {
        panic!("expected a card-name choice, got {choice_type:?}");
    };
    if domain.is_empty() && prompt_options.is_empty() {
        assert!(
            runner
                .state()
                .all_card_names
                .iter()
                .any(|legal| legal.eq_ignore_ascii_case(named)),
            "CR 201.4: the open prompt's legal domain is the Oracle card reference, \
             which must include {named}"
        );
    } else {
        assert!(
            domain
                .iter()
                .chain(prompt_options.iter())
                .any(|legal| legal.eq_ignore_ascii_case(named)),
            "the offered card-name options must include {named}"
        );
    }
    assert!(
        !runner.state().battlefield.contains(&permanent),
        "CR 614.1c: the choice is made before the permanent enters"
    );

    runner
        .act(GameAction::ChooseOption {
            choice: named.to_string(),
        })
        .expect("naming a printed card must be accepted");

    assert!(
        runner.state().battlefield.contains(&permanent),
        "the permanent enters after the name is chosen"
    );
    assert_eq!(
        runner.state().objects[&permanent].chosen_card_name(),
        Some(named),
        "the chosen card name persists on the permanent"
    );
}

/// The pre-existing tests name [`NAMED_CARD`].
fn cast_naming_grizzly_bears(runner: &mut GameRunner, intervention: ObjectId) {
    cast_naming(runner, intervention, NAMED_CARD);
}

/// Completion boundary: combat damage was dealt and the game settled back into
/// a clean priority window with an empty stack.
fn assert_combat_settled(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "combat must settle into a priority window, got {:?}",
        state.waiting_for
    );
    assert!(
        state.stack.is_empty(),
        "nothing may remain on the stack after combat damage"
    );
    assert!(
        matches!(state.phase, Phase::EndCombat | Phase::PostCombatMain),
        "combat damage must have been dealt, phase is {:?}",
        state.phase
    );
}

/// CR 709.4a: the committed fixture must retain both printed halves, the Fuse
/// keyword, and a clean (unfused) marker; a missing
/// `rehydrate_game_from_card_db` fails here, loudly.
fn assert_toil_trouble_loaded(runner: &GameRunner, toil: ObjectId) {
    let card = &runner.state().objects[&toil];
    assert_eq!(card.name, "Toil", "the split card loads as its front half");
    assert_eq!(
        card.back_face.as_ref().map(|face| face.name.as_str()),
        Some("Trouble"),
        "the real database fixture must retain Trouble as the split half"
    );
    assert!(
        card.keywords.contains(&Keyword::Fuse),
        "the printed Fuse keyword must be on the card: {:?}",
        card.keywords
    );
    assert!(
        !card.fused_split_spell,
        "a freshly loaded card carries no fused marker"
    );
}

/// Announce a cast and require the casting-variant menu for the caster and the
/// announced card, returning its options.
fn open_cast_menu(
    runner: &mut GameRunner,
    caster: PlayerId,
    card: ObjectId,
) -> Vec<CastingVariantChoiceOption> {
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|error| panic!("{caster:?} must be able to announce {card:?}: {error:?}"));
    let WaitingFor::CastingVariantChoice {
        player,
        object_id,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the casting menu must be offered to {caster:?}, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, caster,
        "only the caster chooses the casting variant"
    );
    assert_eq!(object_id, card, "the menu is for the announced card");
    options
}

/// CR 601.2b + CR 702.102c: assert the menu by `(variant, face, mana value)`
/// tuples, never by index or order. A Fuse option must also be the fused
/// {4}{B}{R} cost, so its shards carry Black and Red.
fn assert_menu(
    options: &[CastingVariantChoiceOption],
    expected: &[(CastingVariant, CastingVariantFace, u32)],
) {
    assert_eq!(
        options.len(),
        expected.len(),
        "the menu must offer exactly {expected:?}, got {options:?}"
    );
    for &(variant, face, value) in expected {
        let matching: Vec<&CastingVariantChoiceOption> = options
            .iter()
            .filter(|option| option.variant == variant && option.face == face)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "the menu must contain exactly one {variant:?}/{face:?} option: {options:?}"
        );
        assert_eq!(
            matching[0].mana_cost.mana_value(),
            value,
            "the {variant:?}/{face:?} option's mana value: {options:?}"
        );
    }
    for option in options {
        if option.variant == CastingVariant::Fuse {
            assert_eq!(
                option.face,
                CastingVariantFace::Left,
                "CR 702.102a: a fused split cast is announced on the left half"
            );
            let ManaCost::Cost { shards, .. } = &option.mana_cost else {
                panic!(
                    "CR 702.102c: the fused option must carry a concrete mana cost, got {:?}",
                    option.mana_cost
                );
            };
            assert!(
                shards.contains(&ManaCostShard::Black),
                "the fused cost {option:?} includes Toil's {{B}}"
            );
            assert!(
                shards.contains(&ManaCostShard::Red),
                "the fused cost {option:?} includes Trouble's {{R}}"
            );
        }
    }
}

/// Select the unique offered `(variant, face)` option by index.
fn choose_option(
    runner: &mut GameRunner,
    options: &[CastingVariantChoiceOption],
    variant: CastingVariant,
    face: CastingVariantFace,
) {
    let matching: Vec<usize> = options
        .iter()
        .enumerate()
        .filter(|(_, option)| option.variant == variant && option.face == face)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "the menu must contain exactly one {variant:?}/{face:?} option: {options:?}"
    );
    runner
        .act(GameAction::ChooseCastingVariant { index: matching[0] })
        .expect("the offered casting variant is selectable");
}

/// CR 601.2c: answer one `TargetSelection` prompt per slot, asserting the
/// caster and that both players are legal; the requested target must be among
/// the slot's legal targets.
fn answer_targets(runner: &mut GameRunner, caster: PlayerId, targets: &[TargetRef]) {
    for (slot, target) in targets.iter().enumerate() {
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected TargetSelection for target slot {slot} from {caster:?}, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, caster, "the caster chooses the targets");
        let legal = &target_slots[selection.current_slot].legal_targets;
        assert!(
            legal.contains(&TargetRef::Player(P0)),
            "slot {slot} must offer P0 as a legal target: {legal:?}"
        );
        assert!(
            legal.contains(&TargetRef::Player(P1)),
            "slot {slot} must offer P1 as a legal target: {legal:?}"
        );
        assert!(
            legal.contains(target),
            "the requested target {target:?} must be legal in slot {slot}: {legal:?}"
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(target.clone()),
            })
            .expect("the requested target is selected");
    }
}

/// CR 601.2a + CR 702.102c: the cast committed to the stack, at the caster's
/// priority, as the declared variant and for the declared total cost.
fn assert_on_stack(runner: &GameRunner, caster: PlayerId, variant: CastingVariant, spent: u32) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == caster),
        "the cast must complete to {caster:?}'s priority, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.stack.len(),
        1,
        "exactly the announced spell is on the stack"
    );
    let entry = state
        .stack
        .last()
        .expect("the announced spell is on the stack");
    let StackEntryKind::Spell {
        casting_variant,
        actual_mana_spent,
        ..
    } = &entry.kind
    else {
        panic!("expected a spell stack entry, got {:?}", entry.kind);
    };
    assert_eq!(*casting_variant, variant, "the declared casting variant");
    assert_eq!(*actual_mana_spent, spent, "the total mana paid");
}

/// Pass priority until the stack is empty, collecting each action's events so a
/// test can tie its assertions to the damage event itself.
fn resolve_collecting_events(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..8 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return events;
        }
        let WaitingFor::Priority { .. } = runner.state().waiting_for else {
            panic!(
                "the spell must resolve without another prompt, got {:?}",
                runner.state().waiting_for
            );
        };
        let result = runner
            .act(GameAction::PassPriority)
            .expect("pass priority while resolving");
        events.extend(result.events);
    }
    panic!("the stack never emptied");
}

/// The damage and prevention events attributable to `source` against `target`.
fn damage_events(
    events: &[GameEvent],
    source: ObjectId,
    target: &TargetRef,
) -> (Vec<u32>, Vec<u32>) {
    let dealt = events
        .iter()
        .filter_map(|event| {
            if let GameEvent::DamageDealt {
                source_id,
                target: victim,
                amount,
                ..
            } = event
            {
                if *source_id == source && victim == target {
                    return Some(*amount);
                }
            }
            None
        })
        .collect();
    let prevented = events
        .iter()
        .filter_map(|event| {
            if let GameEvent::DamagePrevented {
                source_id,
                target: victim,
                amount,
            } = event
            {
                if *source_id == source && victim == target {
                    return Some(*amount);
                }
            }
            None
        })
        .collect();
    (dealt, prevented)
}

/// The number of cards in `player`'s hand.
fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|seat| seat.id == player)
        .unwrap_or_else(|| panic!("{player:?} is seated"))
        .hand
        .len()
}

/// Add `kinds` to `player`'s mana pool, in order.
fn add_mana(runner: &mut GameRunner, player: PlayerId, kinds: &[ManaType]) {
    let seat = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|seat| seat.id == player)
        .unwrap_or_else(|| panic!("{player:?} is seated"));
    for &kind in kinds {
        let unit = mana(kind, 1).pop().expect("one mana unit");
        seat.mana_pool.add(unit);
    }
}

/// CR 609.7c + CR 615.1a + CR 120.3a: damage from a source with the chosen name
/// is prevented when the recipient is P0 or a permanent P0 controls. A source
/// with a different name damages P0 in the same combat.
#[test]
fn gideons_intervention_prevents_chosen_name_damage_to_its_controller() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let intervention = scenario.add_real_card(P0, "Gideon's Intervention", Zone::Hand, db);
    let blocker = scenario.add_vanilla(P0, 2, 2);
    let named_one = scenario.add_real_card(P1, NAMED_CARD, Zone::Battlefield, db);
    let named_two = scenario.add_real_card(P1, NAMED_CARD, Zone::Battlefield, db);
    let other = scenario.add_real_card(P1, "Hill Giant", Zone::Battlefield, db);
    let mut pool = mana(ManaType::White, 2);
    pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    cast_naming_grizzly_bears(&mut runner, intervention);

    begin_p1_attack_step(&mut runner);
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (named_one, AttackTarget::Player(P0)),
            (named_two, AttackTarget::Player(P0)),
            (other, AttackTarget::Player(P0)),
        ])
        .expect("P1 attacks with both Grizzly Bears and Hill Giant");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, named_one)])
        .expect("P0 blocks one Grizzly Bears with the 2/2");
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.life(P0),
        17,
        "only Hill Giant (a non-matching source) damages P0; both Grizzly Bears' \
         combat damage is prevented"
    );
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        0,
        "the named attacker's combat damage to a permanent P0 controls is prevented"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "reach: the 2/2 blocker survives"
    );
    assert_eq!(
        runner.state().objects[&named_one].zone,
        Zone::Graveyard,
        "reach guard: the shield is one-way — the blocker's damage to the named \
         attacker is not prevented"
    );
    assert_eq!(
        runner.state().objects[&named_two].zone,
        Zone::Battlefield,
        "reach guard: the unblocked named attacker survives its prevented attack"
    );
    assert_eq!(
        runner.life(P1),
        20,
        "reach guard: P1 takes no combat damage in this combat"
    );
}

/// CR 609.7c: the prevention's recipient scope is "you and permanents you
/// control". Damage dealt by the named source to P1 (life) and to P1's own
/// creature is dealt in full.
#[test]
fn gideons_intervention_does_not_prevent_chosen_name_damage_to_opponents() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let intervention = scenario.add_real_card(P0, "Gideon's Intervention", Zone::Hand, db);
    let named_one = scenario.add_real_card(P0, NAMED_CARD, Zone::Battlefield, db);
    let named_two = scenario.add_real_card(P0, NAMED_CARD, Zone::Battlefield, db);
    let p1_blocker = scenario.add_vanilla(P1, 0, 2);
    let mut pool = mana(ManaType::White, 2);
    pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    cast_naming_grizzly_bears(&mut runner, intervention);

    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (named_one, AttackTarget::Player(P1)),
            (named_two, AttackTarget::Player(P1)),
        ])
        .expect("P0 attacks with both Grizzly Bears");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(p1_blocker, named_one)])
        .expect("P1 blocks one Grizzly Bears");
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.life(P1),
        18,
        "damage dealt by the chosen-name source to its controller's opponent is \
         not a protected recipient, so the unblocked 2 is dealt"
    );
    assert_eq!(
        runner.state().objects[&p1_blocker].zone,
        Zone::Graveyard,
        "damage dealt by the chosen-name source to P1's own creature is not prevented: \
         the 0/2 blocker took 2 damage and died"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: P0's own creatures are the only sources here"
    );
}

/// CR 101.2 + CR 201.2 + CR 604.1: the second line prohibits P1 from casting a
/// spell with the chosen name, while a differently named creature spell is
/// still castable.
#[test]
fn gideons_intervention_blocks_the_named_spell_but_not_another() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let intervention = scenario.add_real_card(P0, "Gideon's Intervention", Zone::Hand, db);
    let named_spell = scenario.add_real_card(P1, NAMED_CARD, Zone::Hand, db);
    let other_spell = scenario.add_real_card(P1, "Centaur Courser", Zone::Hand, db);
    let mut p0_pool = mana(ManaType::White, 2);
    p0_pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, p0_pool);
    let mut p1_pool = mana(ManaType::Green, 1);
    p1_pool.extend(mana(ManaType::Colorless, 3));
    scenario.with_mana_pool(P1, p1_pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    cast_naming_grizzly_bears(&mut runner, intervention);

    // P1's own main phase so the creature cards are castable at sorcery speed.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let named_card_id = runner.state().objects[&named_spell].card_id;
    let rejected = runner.act(GameAction::CastSpell {
        object_id: named_spell,
        card_id: named_card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    let Err(EngineError::ActionNotAllowed(message)) = &rejected else {
        panic!("CR 101.2: the chosen-name spell must be prohibited, got {rejected:?}");
    };
    assert_eq!(
        message, "A static ability prevents you from casting this spell",
        "CR 101.2: the prohibition, not another prerequisite, must reject the cast"
    );
    assert_eq!(
        runner.state().objects[&named_spell].zone,
        Zone::Hand,
        "the prohibited spell stays in hand"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the prohibited cast puts nothing on the stack"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "the rejection leaves P1 holding priority, got {:?}",
        runner.state().waiting_for
    );

    let other_card_id = runner.state().objects[&other_spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: other_spell,
            card_id: other_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("a differently named creature spell is not prohibited");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&other_spell].zone,
        Zone::Battlefield,
        "the differently named creature spell resolves onto the battlefield"
    );
    assert_eq!(
        runner.state().objects[&other_spell].controller,
        P1,
        "the cast spell is controlled by its caster"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ),
        "the game settles with priority for the active player, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing may remain on the stack"
    );
}

/// The fused-prevention board: P0 holds the Intervention and the real split card, P0
/// has the Intervention's {2}{W}{W} and enough for the fused {4}{B}{R}, and
/// three real Grizzly Bears are in P0's library so Toil has cards to draw.
fn fused_prevention_board(name: &str) -> (GameRunner, ObjectId, ObjectId) {
    let db = db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let intervention = scenario.add_real_card(P0, "Gideon's Intervention", Zone::Hand, db);
    let toil = scenario.add_real_card(P0, "Toil", Zone::Hand, db);
    for _ in 0..3 {
        let _ = scenario.add_real_card(P0, NAMED_CARD, Zone::Library, db);
    }
    let mut p0_pool = mana(ManaType::White, 2);
    p0_pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, p0_pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_toil_trouble_loaded(&runner, toil);
    cast_naming(&mut runner, intervention, name);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "P0 keeps priority in its own precombat main, got {:?}",
        runner.state().waiting_for
    );
    add_mana(
        &mut runner,
        P0,
        &[
            ManaType::Black,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    (runner, intervention, toil)
}

/// CR 601.2b: the printed Fuse option's {4}{B}{R}, the two unfused halves' {2}{B}
/// and {2}{R}, each cast from hand.
fn assert_toil_trouble_menu(options: &[CastingVariantChoiceOption]) {
    assert_menu(
        options,
        &[
            (CastingVariant::Normal, CastingVariantFace::Left, 3),
            (CastingVariant::Normal, CastingVariantFace::Right, 3),
            (CastingVariant::Fuse, CastingVariantFace::Left, 6),
        ],
    );
}

/// CR 709.4a + CR 709.4d + CR 702.102b + CR 609.7c + CR 615.2: the fused
/// Toil // Trouble carries both halves' names, so Gideon's Intervention naming
/// "Trouble" prevents Trouble's damage although the card's front name is
/// "Toil". Toil still resolves (P0 draws two and loses 2 life).
#[test]
fn gideons_intervention_prevents_fused_split_spells_right_half_damage() {
    let (mut runner, intervention, toil) = fused_prevention_board("Trouble");

    let options = open_cast_menu(&mut runner, P0, toil);
    assert_toil_trouble_menu(&options);
    choose_option(
        &mut runner,
        &options,
        CastingVariant::Fuse,
        CastingVariantFace::Left,
    );
    answer_targets(
        &mut runner,
        P0,
        &[TargetRef::Player(P0), TargetRef::Player(P0)],
    );
    assert_on_stack(&runner, P0, CastingVariant::Fuse, 6);
    assert!(
        runner.state().objects[&toil].fused_split_spell,
        "the fused cast carries the both-halves marker on the stack"
    );
    let events = resolve_collecting_events(&mut runner);

    let (dealt, prevented) = damage_events(&events, toil, &TargetRef::Player(P0));
    assert!(
        dealt.is_empty(),
        "Trouble's damage must be prevented, but these were dealt: {dealt:?}"
    );
    assert_eq!(
        prevented,
        vec![2],
        "CR 615.2: the shield prevents Trouble's 2 damage (P0's hand after Toil)"
    );
    assert_eq!(
        runner.life(P0),
        18,
        "only Toil's 2 life loss applies; Trouble's 2 damage is prevented"
    );
    assert_eq!(
        hand_size(&runner, P0),
        2,
        "reach guard: Toil resolved and drew two cards"
    );
    assert_eq!(
        runner.state().objects[&toil].zone,
        Zone::Graveyard,
        "the fused spell resolved"
    );
    assert!(
        runner.state().battlefield.contains(&intervention),
        "the shield's source remains on the battlefield"
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing may remain on the stack"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the game settles with priority for the active player, got {:?}",
        runner.state().waiting_for
    );
}

/// Control: naming the front half "Toil" also covers the fused spell, because a
/// fused split spell has both names (CR 709.4d). The prevention and the fused
/// pipeline both reach the same damage.
#[test]
fn gideons_intervention_front_half_named_prevents_fused_damage() {
    let (mut runner, _intervention, toil) = fused_prevention_board("Toil");

    let options = open_cast_menu(&mut runner, P0, toil);
    assert_toil_trouble_menu(&options);
    choose_option(
        &mut runner,
        &options,
        CastingVariant::Fuse,
        CastingVariantFace::Left,
    );
    answer_targets(
        &mut runner,
        P0,
        &[TargetRef::Player(P0), TargetRef::Player(P0)],
    );
    assert_on_stack(&runner, P0, CastingVariant::Fuse, 6);
    let events = resolve_collecting_events(&mut runner);

    let (dealt, prevented) = damage_events(&events, toil, &TargetRef::Player(P0));
    assert!(
        dealt.is_empty(),
        "the front name covers the fused spell, so no damage may be dealt: {dealt:?}"
    );
    assert_eq!(prevented, vec![2], "CR 615.2: the 2 damage is prevented");
    assert_eq!(runner.life(P0), 18, "Toil's life loss only");
    assert_eq!(
        hand_size(&runner, P0),
        2,
        "reach guard: Toil resolved and drew two cards"
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing may remain on the stack"
    );
}

/// Hostile control: a name neither half has leaves the fused spell's damage
/// untouched. The damage event itself proves the shield was not missing a
/// damage step, so the 16 life is the unshielded outcome.
#[test]
fn gideons_intervention_unrelated_name_does_not_prevent_fused_damage() {
    let (mut runner, _intervention, toil) = fused_prevention_board(NAMED_CARD);

    let options = open_cast_menu(&mut runner, P0, toil);
    assert_toil_trouble_menu(&options);
    choose_option(
        &mut runner,
        &options,
        CastingVariant::Fuse,
        CastingVariantFace::Left,
    );
    answer_targets(
        &mut runner,
        P0,
        &[TargetRef::Player(P0), TargetRef::Player(P0)],
    );
    assert_on_stack(&runner, P0, CastingVariant::Fuse, 6);
    let events = resolve_collecting_events(&mut runner);

    let (dealt, prevented) = damage_events(&events, toil, &TargetRef::Player(P0));
    assert_eq!(
        dealt,
        vec![2],
        "Trouble deals 2 to P0: no chosen name matches the fused spell"
    );
    assert!(
        prevented.is_empty(),
        "no prevention effect applies here: {prevented:?}"
    );
    assert_eq!(runner.life(P0), 16, "20 - Toil's 2 - Trouble's 2");
    assert_eq!(
        hand_size(&runner, P0),
        2,
        "reach guard: Toil resolved and drew two cards"
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing may remain on the stack"
    );
}

/// CR 709.3b: an unfused right-half cast puts only Trouble on the stack, whose
/// name is "Trouble", not the chosen "Toil" — even though the off-stack card's
/// other half is Toil, the unfused spell does not answer to that name.
#[test]
fn gideons_intervention_unfused_right_half_does_not_have_the_other_half_name() {
    let db = db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let intervention = scenario.add_real_card(P0, "Gideon's Intervention", Zone::Hand, db);
    let toil = scenario.add_real_card(P0, "Toil", Zone::Hand, db);
    for _ in 0..2 {
        let _ = scenario.add_real_card(P0, NAMED_CARD, Zone::Hand, db);
    }
    let mut p0_pool = mana(ManaType::White, 2);
    p0_pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, p0_pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_toil_trouble_loaded(&runner, toil);
    cast_naming(&mut runner, intervention, "Toil");
    add_mana(
        &mut runner,
        P0,
        &[
            ManaType::Black,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );

    let options = open_cast_menu(&mut runner, P0, toil);
    assert_toil_trouble_menu(&options);
    choose_option(
        &mut runner,
        &options,
        CastingVariant::Normal,
        CastingVariantFace::Right,
    );
    answer_targets(&mut runner, P0, &[TargetRef::Player(P0)]);
    assert_on_stack(&runner, P0, CastingVariant::Normal, 3);
    assert_eq!(
        runner.state().objects[&toil].name,
        "Trouble",
        "CR 709.3b: only the right half exists on the stack"
    );
    assert!(
        !runner.state().objects[&toil].fused_split_spell,
        "the unfused cast has no fused marker"
    );
    let events = resolve_collecting_events(&mut runner);

    let (dealt, prevented) = damage_events(&events, toil, &TargetRef::Player(P0));
    assert_eq!(
        dealt,
        vec![2],
        "Trouble's name is not the chosen \"Toil\", so its 2 damage is dealt"
    );
    assert!(
        prevented.is_empty(),
        "the chosen name does not cover an unfused right half: {prevented:?}"
    );
    assert_eq!(runner.life(P0), 18, "20 - Trouble's 2");
    assert_eq!(
        hand_size(&runner, P0),
        2,
        "the two hand cards were never drawn"
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing may remain on the stack"
    );
}

/// The opponent-casting board: P0 has the naming permanent in hand and the {2}{W}{W} to
/// cast it; P1 holds the real Toil // Trouble with the fused {4}{B}{R} and two
/// real Grizzly Bears to draw.
fn opponent_chosen_name_board(permanent: &str, name: &str) -> (GameRunner, ObjectId, ObjectId) {
    let db = db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let naming = scenario.add_real_card(P0, permanent, Zone::Hand, db);
    let toil = scenario.add_real_card(P1, "Toil", Zone::Hand, db);
    for _ in 0..2 {
        let _ = scenario.add_real_card(P1, NAMED_CARD, Zone::Library, db);
    }
    let mut p0_pool = mana(ManaType::White, 2);
    p0_pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, p0_pool);
    let mut p1_pool = mana(ManaType::Black, 1);
    p1_pool.extend(mana(ManaType::Red, 1));
    p1_pool.extend(mana(ManaType::Colorless, 4));
    scenario.with_mana_pool(P1, p1_pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_toil_trouble_loaded(&runner, toil);
    cast_naming(&mut runner, naming, name);
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, naming, toil)
}

/// CR 101.2 + CR 709.4d + CR 702.102b + CR 601.3: with Trouble named, P1 may
/// not cast the fused pair (it would carry Trouble's name) nor the right half,
/// but the un-chosen left half Toil stays castable and resolves.
#[test]
fn gideons_intervention_opponent_cannot_cast_fused_when_right_half_named() {
    let (mut runner, _intervention, toil) =
        opponent_chosen_name_board("Gideon's Intervention", "Trouble");

    let options =
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P1, toil);
    assert_menu(
        &options,
        &[(CastingVariant::Normal, CastingVariantFace::Left, 3)],
    );

    let card_id = runner.state().objects[&toil].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: toil,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the un-chosen left half is still castable");
    answer_targets(&mut runner, P1, &[TargetRef::Player(P1)]);
    assert_on_stack(&runner, P1, CastingVariant::Normal, 3);
    assert!(
        !runner.state().objects[&toil].fused_split_spell,
        "the sole offered option was the unfused left half"
    );
    let _ = resolve_collecting_events(&mut runner);

    assert_eq!(
        hand_size(&runner, P1),
        2,
        "reach guard: Toil resolved and drew two cards"
    );
    assert_eq!(runner.life(P1), 18, "Toil's 2 life loss");
    assert_eq!(
        runner.state().objects[&toil].zone,
        Zone::Graveyard,
        "the left half resolved and went to the graveyard"
    );
}

/// CR 101.2 + CR 709.4a: with Toil named, only the right half Trouble is
/// castable; the fused pair would carry Toil's name too.
#[test]
fn gideons_intervention_opponent_front_half_named_allows_only_right_half() {
    let (mut runner, _intervention, toil) =
        opponent_chosen_name_board("Gideon's Intervention", "Toil");

    let options =
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P1, toil);
    assert_menu(
        &options,
        &[(CastingVariant::Normal, CastingVariantFace::Right, 3)],
    );

    let card_id = runner.state().objects[&toil].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: toil,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the un-chosen right half is still castable");
    answer_targets(&mut runner, P1, &[TargetRef::Player(P1)]);
    assert_on_stack(&runner, P1, CastingVariant::Normal, 3);
    assert_eq!(
        runner.state().objects[&toil].name,
        "Trouble",
        "CR 709.3b: the right half is the only half on the stack"
    );
    assert!(
        !runner.state().objects[&toil].fused_split_spell,
        "the unfused cast has no fused marker"
    );
}

/// Hostile control: an unrelated chosen name leaves all three printed casts
/// available to the opponent.
#[test]
fn gideons_intervention_opponent_unrelated_name_offers_all_three_casts() {
    let (mut runner, _intervention, toil) =
        opponent_chosen_name_board("Gideon's Intervention", NAMED_CARD);

    let options = open_cast_menu(&mut runner, P1, toil);
    assert_toil_trouble_menu(&options);
}

/// CR 709.4a + CR 702.102b: the persisted `fused_split_spell` marker can be
/// stale (a resolved fused cast leaves it set), but the cast-time prohibition
/// must read the announced variant, not the marker: naming "Trouble" prohibits
/// the fused cast while the un-chosen left half Toil stays castable.
#[test]
fn gideons_intervention_stale_fused_marker_does_not_block_the_unfused_left_half() {
    let db = db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let intervention = scenario.add_real_card(P0, "Gideon's Intervention", Zone::Hand, db);
    let toil = scenario.add_real_card(P1, "Toil", Zone::Hand, db);
    for _ in 0..2 {
        let _ = scenario.add_real_card(P1, NAMED_CARD, Zone::Library, db);
    }
    let mut p0_pool = mana(ManaType::White, 2);
    p0_pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, p0_pool);
    let mut p1_pool = mana(ManaType::Black, 1);
    p1_pool.extend(mana(ManaType::Red, 1));
    p1_pool.extend(mana(ManaType::Colorless, 4));
    scenario.with_mana_pool(P1, p1_pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_toil_trouble_loaded(&runner, toil);
    // The state a resolved fused cast plus Regrowth leaves: a hand card whose
    // marker is still set; no zone move clears it.
    runner
        .state_mut()
        .objects
        .get_mut(&toil)
        .expect("the split card is loaded")
        .fused_split_spell = true;
    cast_naming(&mut runner, intervention, "Trouble");
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let options =
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P1, toil);
    assert_menu(
        &options,
        &[(CastingVariant::Normal, CastingVariantFace::Left, 3)],
    );

    let card_id = runner.state().objects[&toil].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: toil,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the stale marker must not prohibit the un-chosen left half");
    answer_targets(&mut runner, P1, &[TargetRef::Player(P1)]);
    assert_on_stack(&runner, P1, CastingVariant::Normal, 3);
    assert!(
        !runner.state().objects[&toil].fused_split_spell,
        "CR 400.7: the cast is priced and finalized as unfused"
    );
}

/// CR 709.4a + CR 702.102c + CR 601.2f: Disruptor Flute naming "Trouble" taxes
/// the fused cast ({4}{B}{R} + {3} = 9) and the right half ({2}{R} + {3} = 6),
/// while the un-chosen left half Toil stays at {2}{B}. The payment agrees with
/// the menu.
#[test]
fn disruptor_flute_taxes_a_fused_cast_when_the_right_half_is_named() {
    let db = db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flute = scenario.add_real_card(P0, "Disruptor Flute", Zone::Hand, db);
    let toil = scenario.add_real_card(P1, "Toil", Zone::Hand, db);
    for _ in 0..2 {
        let _ = scenario.add_real_card(P1, NAMED_CARD, Zone::Library, db);
    }
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 2));
    let mut p1_pool = mana(ManaType::Black, 1);
    p1_pool.extend(mana(ManaType::Red, 1));
    p1_pool.extend(mana(ManaType::Colorless, 7));
    scenario.with_mana_pool(P1, p1_pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_toil_trouble_loaded(&runner, toil);
    cast_naming(&mut runner, flute, "Trouble");
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let options = open_cast_menu(&mut runner, P1, toil);
    assert_menu(
        &options,
        &[
            (CastingVariant::Normal, CastingVariantFace::Left, 3),
            (CastingVariant::Normal, CastingVariantFace::Right, 6),
            (CastingVariant::Fuse, CastingVariantFace::Left, 9),
        ],
    );
    choose_option(
        &mut runner,
        &options,
        CastingVariant::Fuse,
        CastingVariantFace::Left,
    );
    answer_targets(
        &mut runner,
        P1,
        &[TargetRef::Player(P1), TargetRef::Player(P1)],
    );
    assert_on_stack(&runner, P1, CastingVariant::Fuse, 9);
    let remaining = runner
        .state()
        .players
        .iter()
        .find(|seat| seat.id == P1)
        .expect("P1 is seated")
        .mana_pool
        .total();
    assert_eq!(
        remaining, 0,
        "the payment-time cost agrees with the taxed menu"
    );
}
