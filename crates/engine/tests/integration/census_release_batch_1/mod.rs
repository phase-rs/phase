//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 1: each shape's Oracle wording runs through
//! the public cast/activate/combat pipeline and behaves as printed. Lines that are not the cited clause (reminder text,
//! Cycling, Scavenge, other triggers) are omitted from the test cards.
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Duration;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn pass_to(runner: &mut GameRunner, want: fn(&WaitingFor) -> bool) {
    for _ in 0..40 {
        if want(&runner.state().waiting_for) {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            o => panic!("unexpected {o:?}"),
        }
    }
    panic!(
        "did not reach wanted state: {:?}",
        runner.state().waiting_for
    );
}

fn attack_and_reach_blockers(r: &mut GameRunner, atk: &[ObjectId]) {
    pass_to(r, |w| matches!(w, WaitingFor::DeclareAttackers { .. }));
    r.act(GameAction::DeclareAttackers {
        attacks: atk.iter().map(|i| (*i, AttackTarget::Player(P1))).collect(),
        bands: vec![],
    })
    .expect("declare attackers");
    pass_to(r, |w| matches!(w, WaitingFor::DeclareBlockers { .. }));
}

fn blocks_ok(r: &GameRunner, blocks: &[(ObjectId, ObjectId)]) -> bool {
    engine::game::combat::validate_blockers_for_player(r.state(), P1, blocks).is_ok()
}

// ---- Shape 1: "This creature must be blocked if able." and the named form (Canopy Stalker, Maarika, Brutal Gladiator,
// Madame Vastra) CR 509.1c ----
#[test]
fn must_be_blocked_if_able() {
    for (name, power, toughness, text) in [
        (
            "Canopy Stalker",
            4,
            2,
            "This creature must be blocked if able.",
        ),
        (
            "Maarika, Brutal Gladiator",
            7,
            4,
            "Maarika, Brutal Gladiator must be blocked if able.",
        ),
        (
            "Madame Vastra",
            3,
            3,
            "Madame Vastra must be blocked if able.",
        ),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let must = s
            .add_creature_from_oracle(P0, name, power, toughness, text)
            .id();
        let plain = s.add_creature(P0, "Plain", 2, 2).id();
        let blocker = s.add_creature(P1, "Blocker", 1, 6).id();
        let mut r = s.build();
        attack_and_reach_blockers(&mut r, &[must, plain]);
        assert!(
            !blocks_ok(&r, &[]),
            "{name}: no block violates the requirement"
        );
        assert!(
            !blocks_ok(&r, &[(blocker, plain)]),
            "{name}: blocking the other attacker violates it"
        );
        assert!(
            blocks_ok(&r, &[(blocker, must)]),
            "{name}: blocking the marked attacker satisfies it"
        );
        r.declare_blockers(&[(blocker, must)])
            .expect("legal block accepted");
    }
}

#[test]
fn must_be_blocked_control_plain_attacker_may_go_unblocked() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let plain = s.add_creature(P0, "Plain", 2, 2).id();
    let _blocker = s.add_creature(P1, "Blocker", 1, 6).id();
    let mut r = s.build();
    attack_and_reach_blockers(&mut r, &[plain]);
    assert!(
        blocks_ok(&r, &[]),
        "control: without the clause no block is fine"
    );
}

// ---- Shape 2: "All creatures able to block this creature do so." (Golgari Decoy ; static) CR 509.1c ----
#[test]
fn all_able_creatures_must_block_static() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let decoy = s
        .add_creature_from_oracle(
            P0,
            "Golgari Decoy",
            2,
            2,
            "All creatures able to block this creature do so.",
        )
        .id();
    let b1 = s.add_creature(P1, "B1", 1, 6).id();
    let b2 = s.add_creature(P1, "B2", 1, 6).id();
    let mut r = s.build();
    attack_and_reach_blockers(&mut r, &[decoy]);
    assert!(!blocks_ok(&r, &[]), "no blockers violates");
    assert!(
        !blocks_ok(&r, &[(b1, decoy)]),
        "one of two able blockers violates"
    );
    assert!(blocks_ok(&r, &[(b1, decoy), (b2, decoy)]), "both block");
    r.declare_blockers(&[(b1, decoy), (b2, decoy)])
        .expect("accepted");
    assert_eq!(
        r.state()
            .combat
            .as_ref()
            .unwrap()
            .blocker_assignments
            .get(&decoy)
            .map(|v| v.len()),
        Some(2)
    );
}

#[test]
fn all_able_creatures_must_block_control_plain_attacker() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let plain = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b1 = s.add_creature(P1, "B1", 1, 6).id();
    let _b2 = s.add_creature(P1, "B2", 1, 6).id();
    let mut r = s.build();
    attack_and_reach_blockers(&mut r, &[plain]);
    assert!(blocks_ok(&r, &[]), "control: no block is fine");
    assert!(
        blocks_ok(&r, &[(b1, plain)]),
        "control: one blocker is fine"
    );
}

// ---- Shape 3: ability "{2}{G}: All creatures able to block this creature this turn do so." (Mortipede) ----
fn mortipede(activate: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let m = s
        .add_creature_from_oracle(
            P0,
            "Mortipede",
            4,
            1,
            "{2}{G}: All creatures able to block this creature this turn do so.",
        )
        .id();
    for _ in 0..3 {
        s.add_basic_land(P0, ManaColor::Green);
    }
    let b1 = s.add_creature(P1, "B1", 1, 6).id();
    let b2 = s.add_creature(P1, "B2", 1, 6).id();
    let mut r = s.build();
    if activate {
        let o = r.activate(m, 0).resolve();
        assert!(o.state().stack.is_empty(), "reach guard: ability resolved");
        assert!(
            r.state()
                .transient_continuous_effects
                .iter()
                .any(|e| e.source_id == m && matches!(e.duration, Duration::UntilEndOfTurn)),
            "the requirement lasts until end of turn"
        );
    }
    attack_and_reach_blockers(&mut r, &[m]);
    (r, m, b1, b2)
}

#[test]
fn all_able_creatures_must_block_control_before_activation() {
    let (r, m, b1, _b2) = mortipede(false);
    assert!(blocks_ok(&r, &[]), "control: no block is fine");
    assert!(blocks_ok(&r, &[(b1, m)]), "control: one blocker is fine");
}

#[test]
fn all_able_creatures_must_block_activated() {
    let (r, m, b1, b2) = mortipede(true);
    assert!(
        !blocks_ok(&r, &[(b1, m)]),
        "only one blocker violates the activated requirement"
    );
    assert!(
        blocks_ok(&r, &[(b1, m), (b2, m)]),
        "both blocking satisfies"
    );
}

// ---- Shape 4: "This creature can block any number of creatures." (Wall of Glare ; "Defender" line kept) CR 509.1a ----
#[test]
fn can_block_any_number_static() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let atk: Vec<ObjectId> = (0..3)
        .map(|i| s.add_creature(P0, &format!("Insect {i}"), 1, 1).id())
        .collect();
    let wall = s
        .add_creature_from_oracle(
            P1,
            "Wall of Glare",
            0,
            5,
            "Defender\nThis creature can block any number of creatures.",
        )
        .id();
    let plain = s.add_creature(P1, "Plain", 1, 6).id();
    let mut r = s.build();
    attack_and_reach_blockers(&mut r, &atk);
    match &r.state().waiting_for {
        WaitingFor::DeclareBlockers {
            block_capacities, ..
        } => {
            assert_eq!(block_capacities.get(&wall), Some(&None), "wall: any number");
            assert_eq!(block_capacities.get(&plain), Some(&Some(1)), "control: one");
        }
        o => panic!("{o:?}"),
    }
    assert!(
        !blocks_ok(&r, &[(plain, atk[0]), (plain, atk[1])]),
        "control rejected"
    );
    r.declare_blockers(&[(wall, atk[0]), (wall, atk[1]), (wall, atk[2])])
        .expect("three blocks accepted");
}

// ---- Shape 5: "Prevent all combat damage that would be dealt this turn." (Lull ; Cycling omitted; Blunt the Assault) ----
fn combat_life(spell: Option<(&str, &str)>) -> (i32, i32) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "Attacker", 2, 2).id();
    let _d = s.add_creature(P1, "Idle", 1, 1).id();
    let spell =
        spell.map(|(name, text)| s.add_spell_to_hand_from_oracle(P0, name, true, text).id());
    let mut r = s.build();
    if let Some(sp) = spell {
        let o = r.cast(sp).resolve();
        assert!(o.state().stack.is_empty(), "reach guard: spell resolved");
    }
    attack_and_reach_blockers(&mut r, &[a]);
    r.declare_blockers(&[]).unwrap();
    r.combat_damage();
    (r.life(P0), r.life(P1))
}

#[test]
fn prevent_all_combat_damage_this_turn() {
    assert_eq!(
        combat_life(None),
        (20, 18),
        "control: 2 combat damage dealt without the spell"
    );
    assert_eq!(
        combat_life(Some((
            "Lull",
            "Prevent all combat damage that would be dealt this turn."
        ))),
        (20, 20),
        "Lull: prevented"
    );
    // Blunt the Assault: two creatures on the battlefield (Attacker, Idle) -> gain 2, no combat damage
    let (p0, p1) = combat_life(Some((
        "Blunt the Assault",
        "You gain 1 life for each creature on the battlefield. Prevent all combat damage that would be dealt this turn.",
    )));
    assert_eq!(
        (p0, p1),
        (22, 20),
        "Blunt the Assault: gain 2 (two creatures), no combat damage"
    );
}

#[test]
fn prevent_all_combat_damage_covers_creatures_but_not_noncombat_damage() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let blocked = s.add_creature(P0, "Blocked", 2, 2).id();
    let unblocked = s.add_creature(P0, "Unblocked", 3, 3).id();
    let blocker = s.add_creature(P1, "Blocker", 2, 2).id();
    let lull = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Lull",
            true,
            "Prevent all combat damage that would be dealt this turn.",
        )
        .id();
    let shock = s
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .id();
    let mut r = s.build();
    r.cast(lull).resolve();
    attack_and_reach_blockers(&mut r, &[blocked, unblocked]);
    r.declare_blockers(&[(blocker, blocked)]).unwrap();
    r.combat_damage();
    for id in [blocked, blocker] {
        let o = &r.state().objects[&id];
        assert_eq!(o.zone, Zone::Battlefield, "blocked pair survives");
        assert_eq!(o.damage_marked, 0, "no combat damage to creatures");
    }
    assert_eq!(r.life(P1), 20, "no combat damage to the player");
    pass_to(&mut r, |w| matches!(w, WaitingFor::Priority { .. }));
    r.cast(shock).target_player(P1).resolve();
    assert_eq!(r.life(P1), 18, "control: noncombat damage is not prevented");
}

// ---- Shape 6: "deals N damage to target creature. If that creature would die this turn, exile it instead." (Feed the Flames) ----
// The rider binds to "that creature" only: a bystander that dies later the same turn still goes to the graveyard.
#[test]
fn damage_then_exile_instead_of_dying() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let victim = s.add_creature(P1, "Victim", 2, 2).id();
    let bystander = s.add_creature(P1, "Bystander", 2, 2).id();
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Feed the Flames",
            true,
            "Feed the Flames deals 5 damage to target creature. If that creature would die this turn, exile it instead.",
        )
        .id();
    let shock = s
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .id();
    let mut r = s.build();
    r.cast(spell).target_object(victim).resolve();
    r.advance_until_stack_empty();
    assert_eq!(
        r.state().objects[&victim].zone,
        Zone::Exile,
        "exiled, not in the graveyard"
    );
    assert!(
        r.state().players[1].graveyard.is_empty(),
        "nothing in P1's graveyard"
    );
    r.cast(shock).target_object(bystander).resolve();
    r.advance_until_stack_empty();
    assert_eq!(
        r.state().objects[&bystander].zone,
        Zone::Graveyard,
        "an unrelated creature that dies the same turn is not exiled"
    );
    assert_eq!(
        r.state().players[1]
            .graveyard
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![bystander],
        "only the bystander is in P1's graveyard"
    );
}

// ---- Shape 7: "If you would gain life, you gain that much life plus 1 instead." (Heron of Hope ; other lines omitted) ----
#[test]
fn gain_life_plus_one() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(
        P0,
        "Heron of Hope",
        2,
        3,
        "If you would gain life, you gain that much life plus 1 instead.",
    );
    let mine = s
        .add_spell_to_hand_from_oracle(P0, "Sacred Nectar", false, "You gain 4 life.")
        .id();
    let theirs = s
        .add_spell_to_hand_from_oracle(P0, "Soothing Balm", true, "Target player gains 5 life.")
        .id();
    let mut r = s.build();
    r.cast(mine).resolve();
    assert_eq!(r.life(P0), 25, "4 + 1");
    r.cast(theirs).target_player(P1).resolve();
    assert_eq!(r.life(P1), 25, "control: opponent's life gain not modified");
}

// ---- Shape 8: "You have hexproof." / "You have shroud." (Spirit of the Hearth, True Believer ; Flying and reminder omitted)
// CR 702.11c / CR 702.18a for players. Asserted on the player targets the cast pipeline offers and on the legality
// predicate behind them; a cast at an illegal player target is not asserted because the scenario harness retargets it
// instead of rejecting it.
use engine::game::targeting::player_is_legal_target;
use engine::types::ability::TargetRef;
use engine::types::player::PlayerId;

/// `holder` controls a creature named `name` with `text` (a vanilla creature when `text` is empty), the other player
/// controls a vanilla creature (so target selection always offers more than one target); P0 holds Shock.
fn shock_scenario(
    holder: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
    text: &str,
) -> (GameRunner, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    if text.is_empty() {
        s.add_creature(holder, name, power, toughness);
    } else {
        s.add_creature_from_oracle(holder, name, power, toughness, text);
    }
    s.add_creature(if holder == P0 { P1 } else { P0 }, "Idle", 1, 1);
    let shock = s
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .id();
    (s.build(), shock)
}

/// Casts P0's Shock without a declared target and returns the players the target selection offers.
fn offered_player_targets(
    holder: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
    text: &str,
) -> Vec<PlayerId> {
    offered_targets(holder, name, power, toughness, text).0
}

/// Casts P0's Shock without a declared target and returns the players and the `holder`'s objects the target selection
/// offers.
fn offered_targets(
    holder: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
    text: &str,
) -> (Vec<PlayerId>, Vec<ObjectId>) {
    let (mut r, shock) = shock_scenario(holder, name, power, toughness, text);
    let card_id = r.state().objects[&shock].card_id;
    r.act(GameAction::CastSpell {
        object_id: shock,
        card_id,
        targets: vec![],
        payment_mode: Default::default(),
    })
    .expect("cast Shock");
    match &r.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            let legal = &target_slots[0].legal_targets;
            (
                legal
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Player(p) => Some(*p),
                        TargetRef::Object(_) => None,
                    })
                    .collect(),
                legal
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Object(o) => {
                            (r.state().objects[o].controller == holder).then_some(*o)
                        }
                        TargetRef::Player(_) => None,
                    })
                    .collect(),
            )
        }
        o => panic!("expected target selection, got {o:?}"),
    }
}

#[test]
fn you_have_hexproof_blocks_opponent_targeting_only() {
    let (offered, objects) =
        offered_targets(P1, "Spirit of the Hearth", 4, 5, "You have hexproof.");
    assert_eq!(offered, vec![P0], "P0's Shock is offered only P0");
    // the grant is to the player only: the holder's own creature stays a legal target for the opponent
    assert_eq!(
        objects.len(),
        1,
        "Spirit of the Hearth itself is offered: {objects:?}"
    );
    let (r, shock) = shock_scenario(P1, "Spirit of the Hearth", 4, 5, "You have hexproof.");
    assert!(
        !player_is_legal_target(r.state(), P1, shock, P0),
        "opponent's spell cannot target the hexproof player"
    );
    assert!(
        player_is_legal_target(r.state(), P1, shock, P1),
        "the hexproof player's own spells can still target them (not shroud)"
    );
    // control: without the static both players are offered and the cast damages P1
    let offered = offered_player_targets(P1, "Grizzly Bears", 2, 2, "");
    assert!(
        offered.len() == 2 && offered.contains(&P0) && offered.contains(&P1),
        "control offers both players: {offered:?}"
    );
    let (mut c, cs) = shock_scenario(P1, "Grizzly Bears", 2, 2, "");
    c.cast(cs).target_player(P1).resolve();
    assert_eq!(c.life(P1), 18, "control: damage dealt");
}

#[test]
fn you_have_shroud_blocks_all_targeting() {
    let (offered, objects) = offered_targets(P0, "True Believer", 2, 2, "You have shroud.");
    assert_eq!(
        offered,
        vec![P1],
        "its controller's own Shock is offered only P1"
    );
    // the grant is to the player only: True Believer itself stays a legal target
    assert_eq!(
        objects.len(),
        1,
        "True Believer itself is offered: {objects:?}"
    );
    let (r, shock) = shock_scenario(P0, "True Believer", 2, 2, "You have shroud.");
    assert!(
        !player_is_legal_target(r.state(), P0, shock, P0),
        "even the controller cannot target a shrouded player"
    );
    assert!(
        !player_is_legal_target(r.state(), P0, shock, P1),
        "nor can an opponent"
    );
    // control: the same holder without the static
    let offered = offered_player_targets(P0, "Grizzly Bears", 2, 2, "");
    assert!(
        offered.len() == 2 && offered.contains(&P0) && offered.contains(&P1),
        "control offers both players: {offered:?}"
    );
}

// ---- Shape 6 variants: other wordings of the same rider ----
/// Shock kills a bystander creature after the spell resolves, in the same turn, and returns its zone: the rider binds
/// per wording, so each wording shows that it exiles only "that creature".
fn shock_bystander(r: &mut GameRunner, bystander: ObjectId, shock: ObjectId) -> Zone {
    r.cast(shock).target_object(bystander).resolve();
    r.advance_until_stack_empty();
    r.state().objects[&bystander].zone
}

/// Returns the victim's zone and the size of P1's graveyard after the spell resolves, then the zone of a bystander
/// that dies later the same turn.
fn exile_victim(
    name: &str,
    is_instant: bool,
    text: &str,
    modes: Option<&[usize]>,
) -> (Zone, usize, Zone) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let victim = s.add_creature(P1, "Victim", 2, 2).id();
    let bystander = s.add_creature(P1, "Bystander", 2, 2).id();
    let id = s
        .add_spell_to_hand_from_oracle(P0, name, is_instant, text)
        .id();
    let shock = s
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .id();
    let mut r = s.build();
    let c = r.cast(id).target_object(victim);
    match modes {
        Some(m) => c.modes(m).resolve(),
        None => c.resolve(),
    };
    r.advance_until_stack_empty();
    let victim_zone = r.state().objects[&victim].zone;
    let graveyard = r.state().players[1].graveyard.len();
    (
        victim_zone,
        graveyard,
        shock_bystander(&mut r, bystander, shock),
    )
}

#[test]
fn exile_rider_wordings() {
    // "that creature or planeswalker" wording with a fixed amount
    assert_eq!(exile_victim("Obliterating Bolt", false, "Obliterating Bolt deals 4 damage to target creature or planeswalker. If that creature or planeswalker would die this turn, exile it instead.", None), (Zone::Exile, 0, Zone::Graveyard));
    // Suplex: the rider sits in the first mode of a "choose one" spell
    assert_eq!(exile_victim("Suplex", false, "Choose one \u{2014}\n\u{2022} Suplex deals 3 damage to target creature. If that creature would die this turn, exile it instead.\n\u{2022} Exile target artifact.", Some(&[0])), (Zone::Exile, 0, Zone::Graveyard));
    // control: no rider -> graveyard
    assert_eq!(
        exile_victim(
            "Concentrated Fire",
            true,
            "Concentrated Fire deals 5 damage to target creature.",
            None
        ),
        (Zone::Graveyard, 1, Zone::Graveyard)
    );
}

// ---- Shape 6, printed wordings: Bouncer's Beatdown, Nine-Ringed Bo, Narset's Rebuke, Khaaaaaaaaaaaannn! ----
/// Returns the victim's zone, its marked damage, and the size of P1's graveyard after the spell or ability resolves,
/// then the zone of a bystander that dies later the same turn.
fn printed_rider(
    name: &str,
    text: &str,
    victim_toughness: i32,
    victim_subtype: Option<&str>,
    artifact_source: bool,
    x: Option<u32>,
    my_power: Option<i32>,
) -> (Zone, u32, usize, Zone) {
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let victim = {
        let mut b = s.add_creature(P1, "Victim", 1, victim_toughness);
        if let Some(st) = victim_subtype {
            b.with_subtypes(vec![st]);
        }
        b.id()
    };
    let bystander = s.add_creature(P1, "Bystander", 2, 2).id();
    let shock = s
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .id();
    if let Some(p) = my_power {
        s.add_creature(P0, "Big", p, p);
    }
    let id = if artifact_source {
        s.add_artifact_from_oracle(P0, name, text).id()
    } else if let Some(xv) = x {
        let id = s
            .add_spell_to_hand_from_oracle(P0, name, true, text)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
                generic: 0,
            })
            .id();
        s.with_mana_pool(
            P0,
            (0..xv)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .chain([ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])])
                .collect(),
        );
        id
    } else {
        s.add_spell_to_hand_from_oracle(P0, name, true, text).id()
    };
    let mut r = s.build();
    if artifact_source {
        r.activate(id, 0).target_object(victim).resolve();
    } else {
        let c = r.cast(id).target_object(victim);
        match x {
            Some(xv) => c.x(xv).resolve(),
            None => c.resolve(),
        };
    }
    r.advance_until_stack_empty();
    let o = &r.state().objects[&victim];
    let (zone, damage) = (o.zone, o.damage_marked);
    let graveyard = r.state().players[1].graveyard.len();
    (
        zone,
        damage,
        graveyard,
        shock_bystander(&mut r, bystander, shock),
    )
}

#[test]
fn exile_rider_printed_wordings() {
    // X is the greatest power among creatures you control (3), so a 1/3 dies and is exiled.
    let (zone, _, gy, bystander) = printed_rider(
        "Bouncer's Beatdown",
        "This spell costs {2} less to cast if it targets a black permanent.\nBouncer's Beatdown deals X damage to target creature or planeswalker, where X is the greatest power among creatures you control. If that creature or planeswalker would die this turn, exile it instead.",
        3,
        None,
        false,
        None,
        Some(3),
    );
    assert_eq!(
        (zone, gy, bystander),
        (Zone::Exile, 0, Zone::Graveyard),
        "Bouncer's Beatdown"
    );
    let (zone, _, gy, bystander) = printed_rider(
        "Nine-Ringed Bo",
        "{T}: This artifact deals 1 damage to target Spirit creature. If that creature would die this turn, exile it instead.",
        1,
        Some("Spirit"),
        true,
        None,
        None,
    );
    assert_eq!(
        (zone, gy, bystander),
        (Zone::Exile, 0, Zone::Graveyard),
        "Nine-Ringed Bo"
    );
    // the mana clause sits between the damage and the rider
    let (zone, _, gy, bystander) = printed_rider(
        "Narset's Rebuke",
        "Narset's Rebuke deals 5 damage to target creature. Add {U}{R}{W}. If that creature would die this turn, exile it instead.",
        2,
        None,
        false,
        None,
        None,
    );
    assert_eq!(
        (zone, gy, bystander),
        (Zone::Exile, 0, Zone::Graveyard),
        "Narset's Rebuke"
    );
    let khan = "Khaaaaaaaaaaaannn! deals twice X damage to target creature. If that creature would die this turn, exile it instead.";
    assert_eq!(
        printed_rider("Khaaaaaaaaaaaannn!", khan, 3, None, false, Some(1), None),
        (Zone::Battlefield, 2, 0, Zone::Graveyard),
        "X = 1: twice X is 2 damage, the 1/3 survives"
    );
    let (zone, _, gy, bystander) =
        printed_rider("Khaaaaaaaaaaaannn!", khan, 3, None, false, Some(2), None);
    assert_eq!(
        (zone, gy, bystander),
        (Zone::Exile, 0, Zone::Graveyard),
        "X = 2: 4 damage, exiled"
    );
}
