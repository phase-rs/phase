//! Regression for GitHub issue #2915 — Alexios, Deimos of Kosmos must scope
//! "can't attack its owner" to the owner player (not blanket CantAttack) and
//! upkeep GiveControl must honor `ScopedPlayer`.

use engine::parser::oracle_static::parse_static_line_multi;
use engine::types::ability::TargetFilter;
use engine::types::statics::StaticMode;
use engine::types::triggers::AttackTargetFilter;

const ALEXIOS_ATTACK_LINE: &str =
    "This creature attacks each combat if able, can't be sacrificed, and can't attack its owner.";

#[test]
fn alexios_cant_attack_owner_is_scoped_not_blanket() {
    let defs = parse_static_line_multi(ALEXIOS_ATTACK_LINE);
    assert_eq!(defs.len(), 3);
    assert_eq!(defs[0].mode, StaticMode::MustAttack);
    assert_eq!(
        defs[1].mode,
        StaticMode::Other("CantBeSacrificed".to_string())
    );
    assert_eq!(defs[2].mode, StaticMode::CantAttack);
    assert_eq!(defs[2].attack_defended, Some(AttackTargetFilter::Owner));
    assert!(defs
        .iter()
        .all(|def| def.affected == Some(TargetFilter::SelfRef)));
}
