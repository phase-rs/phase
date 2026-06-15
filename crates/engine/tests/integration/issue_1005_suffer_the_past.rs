//! Issue #1005 — Suffer the Past must exile from a graveyard, not any zone.

use engine::types::ability::Effect;
use engine::types::zones::Zone;

const ORACLE: &str = "Exile X target cards from target player's graveyard. For each card exiled this way, that player loses 1 life and you gain 1 life.";

#[test]
fn suffer_the_past_parses_graveyard_origin() {
    let parsed = engine::parser::parse_oracle_text(
        ORACLE,
        "Suffer the Past",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = parsed.abilities.first().expect("spell ability");
    let Effect::ChangeZone {
        origin,
        destination,
        ..
    } = ability.effect.as_ref()
    else {
        panic!("expected ChangeZone, got {:?}", ability.effect);
    };
    assert_eq!(
        *origin,
        Some(Zone::Graveyard),
        "Suffer the Past must target cards in a graveyard"
    );
    assert_eq!(*destination, Zone::Exile);
}
