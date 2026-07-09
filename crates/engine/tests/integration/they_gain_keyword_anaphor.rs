//! CR 608.2c + CR 611.2c — the bare plural pronoun "they" as an anaphoric
//! continuous-clause subject in a bare-" and " conjunct.
//!
//! "Put a +1/+1 counter on each [of those] creature[s] and they gain
//! <keyword[s]> until end of turn" back-references the objects the counter
//! clause acted on. Before this fix the conjunct fell through the bare-and
//! clause splitter (which recognized "those creatures" / "that creature" but
//! not the bare pronoun "they"), so `try_split_targeted_compound` fed the
//! conjunct to the imperative-only parser, which emitted
//! `Effect::Unimplemented { name: "they", description: "they gain ..." }`.
//!
//! Splitting the conjunct routes "they gain <kw list>" through
//! `parse_clause_ast` → `try_parse_subject_continuous_clause`, where "they"
//! resolves to `ParentTarget` (CR 608.2c) and the comma-and keyword list lowers
//! to one `AddKeyword` per keyword carrying the grant's duration (CR 611.2c).
//!
//! Cards unlocked: Unbreakable Formation (addendum), Overseer of Vault 76
//! (reflexive remove-counters trigger), and the multi-keyword list form shared
//! with Last Night Together.

use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityDefinition, ContinuousModification, Effect, TargetFilter};
use engine::types::keywords::Keyword;

/// Walk the sub_ability chain and collect every `AddKeyword` granted to an
/// anaphoric `ParentTarget` subject (the "they gain ..." continuous grant).
fn they_gain_keywords(def: &AbilityDefinition) -> Vec<Keyword> {
    let mut cur = Some(def);
    while let Some(d) = cur {
        if let Effect::GenericEffect {
            static_abilities, ..
        } = &*d.effect
        {
            let is_anaphor = static_abilities
                .iter()
                .any(|s| matches!(s.affected, Some(TargetFilter::ParentTarget)));
            if is_anaphor {
                let kws: Vec<Keyword> = static_abilities
                    .iter()
                    .flat_map(|s| s.modifications.iter())
                    .filter_map(|m| match m {
                        ContinuousModification::AddKeyword { keyword } => Some(keyword.clone()),
                        _ => None,
                    })
                    .collect();
                if !kws.is_empty() {
                    return kws;
                }
            }
        }
        cur = d.sub_ability.as_deref();
    }
    Vec::new()
}

/// No clause in the parsed chain may remain `Effect::Unimplemented`.
fn assert_no_unimplemented(def: &AbilityDefinition) {
    let mut cur = Some(def);
    while let Some(d) = cur {
        assert!(
            !matches!(&*d.effect, Effect::Unimplemented { .. }),
            "unexpected Unimplemented clause: {:?}",
            d.effect
        );
        cur = d.sub_ability.as_deref();
    }
}

/// Unbreakable Formation addendum — single-keyword "they gain" after a
/// "put a +1/+1 counter on each of those creatures" conjunct.
#[test]
fn unbreakable_formation_addendum_they_gain_vigilance() {
    let def = parse_effect_chain(
        "Put a +1/+1 counter on each of those creatures and they gain vigilance until end of turn.",
        engine::types::ability::AbilityKind::Spell,
    );
    assert_no_unimplemented(&def);
    assert_eq!(they_gain_keywords(&def), vec![Keyword::Vigilance]);
}

/// Overseer of Vault 76 reflexive clause — "they gain vigilance" after
/// "put a +1/+1 counter on each creature you control".
#[test]
fn overseer_of_vault_76_they_gain_vigilance() {
    let def = parse_effect_chain(
        "Put a +1/+1 counter on each creature you control and they gain vigilance until end of turn.",
        engine::types::ability::AbilityKind::Spell,
    );
    assert_no_unimplemented(&def);
    assert_eq!(they_gain_keywords(&def), vec![Keyword::Vigilance]);
}

/// The multi-keyword comma-and list ("vigilance, indestructible, and haste")
/// after a bare-" and they gain" conjunct emits one AddKeyword per keyword.
#[test]
fn and_they_gain_emits_keyword_list() {
    let def = parse_effect_chain(
        "Put a +1/+1 counter on each creature you control and they gain vigilance, indestructible, and haste until end of turn.",
        engine::types::ability::AbilityKind::Spell,
    );
    assert_no_unimplemented(&def);
    assert_eq!(
        they_gain_keywords(&def),
        vec![Keyword::Vigilance, Keyword::Indestructible, Keyword::Haste]
    );
}
