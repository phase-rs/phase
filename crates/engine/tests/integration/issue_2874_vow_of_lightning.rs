//! Regression for issue #2874: Vow of Lightning — "Enchanted creature gets
//! +2/+2, has first strike, and can't attack you or planeswalkers you control."
//!
//! https://github.com/phase-rs/phase/issues/2874
//!
//! The trailing "and can't attack you or planeswalkers you control" conjunct was
//! silently dropped: the compound static line parsed only the leading +2/+2 and
//! first-strike grant, producing no scoped `CantAttack`. The Vow-cycle lockout
//! was completely inert and the enchanted creature could freely attack its
//! Aura's controller.
//!
//! Fixed on `main` by `try_split_and_cant_attack_scoped` (PR #2986, commit
//! e2b73b7d8). This test locks in the shipped behavior end-to-end through the
//! real parse -> attach -> layer-evaluate -> declare_attackers pipeline.
//!
//! CR 508.1c: a `CantAttack` static carrying `attack_defended` scopes the
//! prohibition to attacks whose `AttackTarget` matches the filter, relative to
//! the static's source controller. CR 611.2c: the effect's controller is the
//! Aura's controller, so the lockout protects that player and their
//! planeswalkers — not other opponents. CR 702.7: first strike.

use engine::game::combat::{declare_attackers, get_valid_attacker_ids, AttackTarget};
use engine::game::layers::evaluate_layers;
use engine::game::zones::create_object;
use engine::parser::oracle_static::parse_static_line_multi;
use engine::types::ability::ContinuousModification;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::AttackTargetFilter;
use engine::types::zones::Zone;

const VOW_OF_LIGHTNING_AURA_LINE: &str =
    "Enchanted creature gets +2/+2, has first strike, and can't attack you or planeswalkers you control.";

const P0: PlayerId = PlayerId(0); // Aura controller (the protected player)
const P1: PlayerId = PlayerId(1); // enchanted creature's controller (active player)
const P2: PlayerId = PlayerId(2); // other opponent

/// PARSE FIDELITY (AST-shape portion): the fixed parser splits the compound line
/// into BOTH the +2/+2 + first-strike continuous grant AND a scoped `CantAttack`
/// companion. The original bug dropped the third conjunct (the attack lockout)
/// entirely, so all three conjuncts are asserted to lock the fix.
#[test]
fn vow_of_lightning_parses_all_three_conjuncts() {
    let defs = parse_static_line_multi(VOW_OF_LIGHTNING_AURA_LINE);

    // Conjunct three: the scoped attack lockout (the conjunct the bug dropped).
    let lockout = defs
        .iter()
        .find(|d| d.mode == StaticMode::CantAttack)
        .unwrap_or_else(|| panic!("expected a CantAttack companion, got {:?}", defs));
    assert_eq!(
        lockout.attack_defended,
        Some(AttackTargetFilter::PlayerOrPlaneswalker),
        "companion must be scoped to PlayerOrPlaneswalker, got {:?}",
        lockout.attack_defended
    );
    assert!(
        lockout.affected.is_some(),
        "CantAttack companion must carry an affected filter (EnchantedBy)"
    );

    // Conjuncts one and two: the +2/+2 and first-strike grant must survive on a
    // single Continuous def carrying all three modifications.
    let grant = defs
        .iter()
        .find(|d| d.mode == StaticMode::Continuous)
        .unwrap_or_else(|| panic!("expected a Continuous grant def, got {:?}", defs));
    let mods = &grant.modifications;
    assert!(
        mods.contains(&ContinuousModification::AddPower { value: 2 }),
        "expected AddPower(2) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddToughness { value: 2 }),
        "expected AddToughness(2) in {mods:?}"
    );
    assert!(
        mods.contains(&ContinuousModification::AddKeyword {
            keyword: Keyword::FirstStrike
        }),
        "expected AddKeyword(FirstStrike) in {mods:?}"
    );
}

/// Build a 3-player board, parse the real Vow of Lightning Oracle line, attach
/// the parsed Aura to a creature P1 controls, and drive the engine through layer
/// evaluation and `declare_attackers`. Proves the lockout is scoped to the Aura
/// controller (P0) — the enchanted creature cannot attack P0 or P0's
/// planeswalker, but CAN attack a different opponent (P2) — AND that the +2/+2
/// and first-strike grant coexist with the restriction (all three conjuncts).
#[test]
fn vow_of_lightning_scoped_lockout_and_buff_end_to_end() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    // CR 508.1: only the active player's creatures may be declared as attackers.
    // The enchanted creature is controlled by P1, so P1 is the active player.
    state.active_player = P1;
    state.turn_number = 2;

    // The enchanted creature P1 controls (base 2/2).
    let attacker_card = CardId(state.next_object_id);
    let attacker = create_object(
        &mut state,
        attacker_card,
        P1,
        "Enchanted Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&attacker).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }

    // The Aura — controlled by P0 (the protected player) — carries the parsed
    // Vow of Lightning static definitions and is attached to the creature.
    // CR 611.2c: the static's source controller (P0) is whom the scoped lockout
    // protects.
    let parsed_defs = parse_static_line_multi(VOW_OF_LIGHTNING_AURA_LINE);
    let aura_card = CardId(state.next_object_id);
    let aura = create_object(
        &mut state,
        aura_card,
        P0,
        "Vow of Lightning".to_string(),
        Zone::Battlefield,
    );
    let ts = state.next_timestamp();
    {
        let aura_obj = state.objects.get_mut(&aura).unwrap();
        aura_obj.card_types.core_types = vec![CoreType::Enchantment];
        aura_obj.card_types.subtypes = vec!["Aura".to_string()];
        aura_obj.base_card_types = aura_obj.card_types.clone();
        aura_obj.attached_to = Some(attacker.into());
        aura_obj.timestamp = ts;
        aura_obj.static_definitions = parsed_defs.clone().into();
        aura_obj.base_static_definitions = std::sync::Arc::new(parsed_defs);
    }
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .attachments
        .push(aura);

    // A planeswalker controlled by P0 (the protected player).
    let pw_card = CardId(state.next_object_id);
    let pw = create_object(
        &mut state,
        pw_card,
        P0,
        "Jace".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&pw)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Planeswalker);

    // (d) After layer evaluation the enchanted creature reflects +2/+2 and has
    // first strike — proving the grant conjuncts coexist with the restriction.
    // CR 613 + CR 702.7.
    evaluate_layers(&mut state);
    assert_eq!(
        state.objects[&attacker].power,
        Some(4),
        "CR 613: Aura's +2/+2 must apply (2 base + 2)"
    );
    assert_eq!(
        state.objects[&attacker].toughness,
        Some(4),
        "CR 613: Aura's +2/+2 must apply (2 base + 2)"
    );
    assert!(
        state.objects[&attacker].has_keyword(&Keyword::FirstStrike),
        "CR 702.7: Aura must grant first strike alongside the restriction"
    );

    // The scoped restriction must NOT remove the creature from attacker
    // eligibility — it can still attack a non-protected opponent (P2).
    assert!(
        get_valid_attacker_ids(&state).contains(&attacker),
        "CR 508.1c: scoped restriction must not remove attacker from eligibility"
    );

    // (b) A clone: the enchanted creature CAN attack a different opponent (P2).
    let mut p2_state = state.clone();
    let mut events = Vec::new();
    assert!(
        declare_attackers(
            &mut p2_state,
            &[(attacker, AttackTarget::Player(P2))],
            &mut events,
        )
        .is_ok(),
        "CR 508.1c: enchanted creature may attack a non-protected opponent (P2)"
    );

    // (a) CR 508.1c: cannot attack the Aura controller (P0).
    events.clear();
    assert!(
        declare_attackers(
            &mut state,
            &[(attacker, AttackTarget::Player(P0))],
            &mut events,
        )
        .is_err(),
        "CR 508.1c: enchanted creature must not attack the Aura controller (P0)"
    );

    // (c) CR 508.1c: cannot attack the Aura controller's planeswalker.
    events.clear();
    assert!(
        declare_attackers(
            &mut state,
            &[(attacker, AttackTarget::Planeswalker(pw))],
            &mut events,
        )
        .is_err(),
        "CR 508.1c: enchanted creature must not attack the Aura controller's planeswalker"
    );
}
