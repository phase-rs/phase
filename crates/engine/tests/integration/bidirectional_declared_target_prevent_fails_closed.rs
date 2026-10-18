//! "Prevent all [combat] damage that would be dealt to and dealt by target
//! <object> this turn" (Cephalid Illusionist, Soratami Cloud Chariot, Kiora the
//! Crashing Wave, Dovin Hand of Control) must not report as supported while the
//! engine cannot scope both halves to the one chosen object.
//!
//! A declared recipient and a mass recipient ("creatures you control") lower to
//! the same `Typed` filter, and the hosted "to" shield keeps that filter as
//! `valid_card`, so it would shield every matching object. The declared form
//! therefore fails closed; the anaphoric form (Maze of Ith) stays supported.
//!
//! CR 601.2c + CR 608.2c + CR 615.1a.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;

const DECLARED: &[(&str, &str)] = &[
    (
        "Cephalid Illusionist",
        "{2}{U}, {T}: Prevent all combat damage that would be dealt to and dealt by target creature you control this turn.",
    ),
    (
        "Kiora, the Crashing Wave",
        "[+1]: Until your next turn, prevent all damage that would be dealt to and dealt by target permanent an opponent controls.",
    ),
];

#[test]
fn declared_target_bidirectional_prevent_is_unsupported() {
    for (name, oracle) in DECLARED {
        let parsed = parse_oracle_text(oracle, name, &[], &[], &[]);
        let ability = parsed
            .abilities
            .first()
            .unwrap_or_else(|| panic!("{name}: expected an ability"));
        assert!(
            matches!(&*ability.effect, Effect::Unimplemented { name, .. } if name == "bidirectional_prevent_declared_target"),
            "{name}: expected the declared bidirectional form to fail closed, got {:?}",
            ability.effect
        );
    }
}
