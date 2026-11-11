//! CR 508.1a + CR 510.1a + CR 702.4b + CR 120.3a/b: `DerivedViews::unblocked_attack_previews` —
//! the engine-owned per-attacker projection of what each creature that may attack would do to a
//! player if it attacks unblocked. Published only while `WaitingFor::DeclareAttackers` is open.
//!
//! Oracle text (Scryfall, verbatim):
//! - Grizzly Bears (2/2): no rules text.
//! - Mirran Crusader (2/2): "Double strike, protection from black and from green"
//! - Glistener Elf (1/1): "Infect (This creature deals damage to creatures in the form of -1/-1
//!   counters and to players in the form of poison counters.)"
//! - Wall of Wood (0/3): "Defender (This creature can't attack.)"
//! - Belligerent Brontodon (4/6): "Each creature you control assigns combat damage equal to its
//!   toughness rather than its power."

use std::collections::HashSet;

use engine::game::combat::AttackTarget;
use engine::game::derived_views::{derive_views, UnblockedAttackPreview};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare attackers");
            }
            ref other => panic!("expected priority or declare attackers, got {other:?}"),
        }
    }
    panic!("did not reach declare attackers");
}

fn valid_attacker_ids(runner: &GameRunner) -> HashSet<ObjectId> {
    let WaitingFor::DeclareAttackers {
        valid_attacker_ids, ..
    } = &runner.state().waiting_for
    else {
        panic!("expected the declare attackers prompt");
    };
    valid_attacker_ids.iter().copied().collect()
}

fn preview(damage: u32, life_loss: u32) -> UnblockedAttackPreview {
    UnblockedAttackPreview { damage, life_loss }
}

/// Plain power, double strike (CR 702.4b), infect (CR 120.3b), and the `valid_attacker_ids`
/// key source (a Defender is absent), then the just-in-time empty map once the prompt closes.
#[test]
fn unblocked_attack_preview_reads_combat_damage_authority_per_attacker() {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let crusader = scenario.add_real_card(P0, "Mirran Crusader", Zone::Battlefield, db);
    let elf = scenario.add_real_card(P0, "Glistener Elf", Zone::Battlefield, db);
    let wall = scenario.add_real_card(P0, "Wall of Wood", Zone::Battlefield, db);

    let mut runner = scenario.build();
    drive_to_declare_attackers(&mut runner);

    let views = derive_views(runner.state(), Some(P0));
    // The wire shape the client's `AttackTargetPicker` reads: a serde rename or skip of the map
    // or its fields would leave the picker showing no projected damage.
    let wire = serde_json::to_value(&views).expect("derived views serialize");
    assert_eq!(
        wire["unblocked_attack_previews"][bears.0.to_string()],
        serde_json::json!({ "damage": 2, "life_loss": 2 }),
    );
    let previews = views.unblocked_attack_previews;
    // Reach guard: the map is keyed by exactly the prompt's attack-eligible creatures.
    assert_eq!(
        previews.keys().copied().collect::<HashSet<_>>(),
        valid_attacker_ids(&runner),
    );
    // CR 510.1a + CR 120.3a: a plain creature assigns its power, all of it life loss.
    assert_eq!(previews.get(&bears), Some(&preview(2, 2)));
    // CR 702.4b: a double striker assigns in both combat damage steps.
    assert_eq!(previews.get(&crusader), Some(&preview(4, 4)));
    // CR 120.3b: infect damage to a player is poison counters, so no life loss.
    assert_eq!(previews.get(&elf), Some(&preview(1, 0)));
    // CR 508.1c + CR 702.3b: a creature with defender can't attack, so it is not previewed.
    assert!(!previews.contains_key(&wall));

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(bears, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare Grizzly Bears attacking P1");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "the attack declaration prompt closed"
    );
    let views = derive_views(runner.state(), Some(P0));
    assert!(views.unblocked_attack_previews.is_empty());
    let json = serde_json::to_string(&views).expect("derived views serialize");
    assert!(!json.contains("unblocked_attack_previews"));
}

/// CR 510.1a as modified by a rule-modifying effect (CR 613.11): Belligerent Brontodon makes
/// each creature its controller controls assign combat damage equal to its toughness.
#[test]
fn unblocked_attack_preview_reads_toughness_damage_rule() {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let brontodon = scenario.add_real_card(P0, "Belligerent Brontodon", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);

    let mut runner = scenario.build();
    drive_to_declare_attackers(&mut runner);

    let previews = derive_views(runner.state(), Some(P0)).unblocked_attack_previews;
    // Brontodon is 4/6: toughness 6, not power 4.
    assert_eq!(previews.get(&brontodon), Some(&preview(6, 6)));
    // Control: Grizzly Bears is 2/2, so toughness and power agree.
    assert_eq!(previews.get(&bears), Some(&preview(2, 2)));
}
