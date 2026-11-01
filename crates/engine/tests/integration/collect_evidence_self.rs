//! Collect evidence never counts the spell being cast as its own evidence.
//!
//! | Card | Seat / zone | Why it is here |
//! |---|---|---|
//! | Detective's Phoenix | P0 graveyard | casts itself from the graveyard with a collect-evidence bestow cost |
//! | Grizzly Bears | P0 battlefield | a creature for the bestowed Phoenix to enchant |
//! | "Evidence N" | P0 graveyard | mana value 3 fodder for the evidence |
//!
//! Oracle (Scryfall), Detective's Phoenix {2}{R}: "Bestow—{R}, Collect evidence 6. (To pay
//! this bestow cost, pay {R} and exile cards with total mana value 6 or greater from your
//! graveyard.) Flying, haste. Enchanted creature gets +2/+2 and has flying and haste. You may
//! cast this card from your graveyard using its bestow ability."
//!
//! CR 601.2a: a spell is moved to the stack before its costs are paid, so the cards that can
//! be exiled as evidence (CR 701.59a) never include it, and it can't count toward the total
//! the castability gate checks (CR 701.59b).

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PHOENIX_ORACLE: &str = "Bestow\u{2014}{R}, Collect evidence 6. (To pay this bestow cost, pay {R} and exile cards with total mana value 6 or greater from your graveyard.)\nFlying, haste\nEnchanted creature gets +2/+2 and has flying and haste.\nYou may cast this card from your graveyard using its bestow ability.";

/// Puts a vanilla card with mana value `mana_value` in P0's graveyard.
fn add_evidence(runner: &mut GameRunner, index: usize, mana_value: u32) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = engine::game::zones::create_object(
        runner.state_mut(),
        card_id,
        P0,
        format!("Evidence {index}"),
        Zone::Graveyard,
    );
    runner.state_mut().objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(mana_value);
    id
}

/// Detective's Phoenix ({2}{R}, mana value 3) in P0's graveyard, Grizzly Bears to enchant,
/// one {R} in the pool, and `fodder` mana value 3 cards.
fn phoenix_in_graveyard(fodder: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    let mut builder = scenario.add_creature_to_graveyard(P0, "Detective's Phoenix", 2, 2);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    });
    builder.with_subtypes(vec!["Phoenix"]);
    builder.with_color(vec![ManaColor::Red]);
    builder.from_oracle_text_with_keywords(&["Flying", "Haste"], PHOENIX_ORACLE);
    let phoenix = builder.id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&phoenix).unwrap();
        for types in [
            &mut obj.card_types.core_types,
            &mut obj.base_card_types.core_types,
        ] {
            if !types.contains(&CoreType::Enchantment) {
                types.push(CoreType::Enchantment);
            }
        }
    }
    let fodder = (0..fodder)
        .map(|i| add_evidence(&mut runner, i, 3))
        .collect();
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Red,
        ObjectId(0),
        false,
        vec![],
    ));
    (runner, phoenix, fodder)
}

fn offered_cast(runner: &GameRunner, id: ObjectId) -> Option<GameAction> {
    legal_actions(runner.state())
        .into_iter()
        .find(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

/// Casts the Phoenix from the graveyard and answers target selection, stopping at the
/// collect-evidence prompt. Returns the cards it offers.
fn cast_phoenix_to_evidence_prompt(runner: &mut GameRunner, phoenix: ObjectId) -> Vec<ObjectId> {
    let card_id = runner.state().objects[&phoenix].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: phoenix,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reach: casting Detective's Phoenix from the graveyard via bestow is accepted");
    for _ in 0..3 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("Grizzly Bears is a legal creature to enchant");
            }
            WaitingFor::CollectEvidenceChoice { cards, .. } => return cards,
            _ => break,
        }
    }
    panic!(
        "reach: expected CollectEvidenceChoice, got {:?}",
        runner.state().waiting_for
    )
}

/// CR 601.2a + CR 701.59a: the evidence prompt during a graveyard cast offers only the
/// other graveyard cards, not the Phoenix being cast.
#[test]
fn graveyard_bestow_evidence_prompt_excludes_the_spell_being_cast() {
    let (mut runner, phoenix, fodder) = phoenix_in_graveyard(2);
    let cards = cast_phoenix_to_evidence_prompt(&mut runner, phoenix);
    assert_eq!(
        cards, fodder,
        "CR 601.2a: the Phoenix is on the stack and isn't its own evidence"
    );
}

/// CR 601.2a + CR 701.59a: submitting the Phoenix itself as evidence is refused, and the
/// cast stays at the evidence prompt with the Phoenix untouched; the other cards then pay,
/// the cast completes and the Phoenix resolves enchanting Grizzly Bears.
#[test]
fn graveyard_bestow_refuses_the_spell_as_its_own_evidence() {
    let (mut runner, phoenix, fodder) = phoenix_in_graveyard(2);
    let offered = cast_phoenix_to_evidence_prompt(&mut runner, phoenix);
    assert_eq!(offered, fodder, "reach: the other cards are offered");
    let result = runner.act(GameAction::SelectCards {
        cards: vec![phoenix, fodder[0]],
    });
    assert!(
        result.is_err(),
        "the spell being cast can't be exiled as its own evidence"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CollectEvidenceChoice { .. }
        ),
        "the evidence prompt is still pending, got {:?}",
        runner.state().waiting_for
    );
    assert_ne!(runner.state().objects[&phoenix].zone, Zone::Exile);
    runner
        .act(GameAction::SelectCards {
            cards: fodder.clone(),
        })
        .expect("the other two cards are valid evidence");
    for id in &fodder {
        assert_eq!(runner.state().objects[id].zone, Zone::Exile);
    }
    // CR 601.2i: the cast completes: the Phoenix is the spell on top of the
    // stack and its caster has priority.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the cast completes with priority, got {:?}",
        runner.state().waiting_for
    );
    let top = runner
        .state()
        .stack
        .last()
        .expect("the Phoenix is on the stack");
    assert!(
        top.id == phoenix && matches!(top.kind, StackEntryKind::Spell { .. }),
        "the Phoenix is the spell on the stack, got {top:?}"
    );
    // CR 702.103b + CR 608.3c: it resolves as an Aura attached to the Bears.
    let bears = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Grizzly Bears")
        .expect("Grizzly Bears is on the battlefield");
    runner.advance_until_stack_empty();
    let resolved = &runner.state().objects[&phoenix];
    assert_eq!(resolved.zone, Zone::Battlefield);
    assert_eq!(
        resolved.attached_to.and_then(|t| t.as_object()),
        Some(bears),
        "the bestowed Phoenix enchants Grizzly Bears"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the game returns to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 701.59b: with one mana value 3 card besides the Phoenix, there isn't evidence 6 to
/// collect, because the Phoenix can't count its own mana value. Control: with two such
/// cards, the same graveyard bestow cast is offered.
#[test]
fn graveyard_bestow_is_not_affordable_by_counting_the_spell_itself_as_evidence() {
    let (control, control_phoenix, _) = phoenix_in_graveyard(2);
    assert!(
        offered_cast(&control, control_phoenix).is_some(),
        "reach: with two mana value 3 cards besides the Phoenix the graveyard bestow cast is offered"
    );

    let (runner, phoenix, _) = phoenix_in_graveyard(1);
    assert_eq!(runner.state().objects[&phoenix].zone, Zone::Graveyard);
    assert!(
        offered_cast(&runner, phoenix).is_none(),
        "CR 701.59b: only mana value 3 of evidence exists besides the Phoenix"
    );
}
