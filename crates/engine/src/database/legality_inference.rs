//! Synthesize format legalities when MTGJSON has not yet populated them for a
//! newly released set. This is a data-pipeline fallback — once MTGJSON carries
//! authoritative legalities, those values win and inference is skipped.
//!
//! Issue #4365: Marvel Super Heroes (MSH) cards shipped with empty `legalities`
//! in AtomicCards while the preview release-gate marked them Banned everywhere.
//! After release, cards must be deck-builder legal even when MTGJSON lags.

use std::collections::BTreeSet;

use super::legality::{CardLegalities, LegalityFormat, LegalityStatus};
use super::mtgjson::LeadershipSkills;
use super::set_catalog::{gated_sets_as_of, SetCatalog};
use crate::types::card::Rarity;

/// Whether MTGJSON left this card without any supported-format legality data.
pub fn needs_inference(legalities: &CardLegalities) -> bool {
    legalities.is_empty()
}

/// Inputs for legality synthesis beyond the normalized MTGJSON map.
pub struct InferenceContext<'a> {
    pub printings: &'a [String],
    pub catalog: &'a SetCatalog,
    pub leadership_skills: Option<&'a LeadershipSkills>,
    pub type_line: Option<&'a str>,
    pub rarities: &'a BTreeSet<Rarity>,
}

/// Infer format legalities for a card whose MTGJSON `legalities` map is empty,
/// but whose printings are exclusively in sets released on or before the
/// generation as-of date.
pub fn infer_missing_legalities(ctx: &InferenceContext<'_>) -> CardLegalities {
    let as_of = gated_sets_as_of();
    if !ctx.catalog.printings_all_released(ctx.printings, as_of) {
        return CardLegalities::new();
    }

    let set_type = ctx
        .catalog
        .dominant_set_type(ctx.printings)
        .unwrap_or("expansion");

    match set_type {
        "funny" | "memorabilia" | "vanguard" | "minigame" | "arcade" | "token" => {
            funny_or_nonconstructible_template()
        }
        "commander" | "spellbook" => commander_product_template(ctx),
        "masters" => masters_template(ctx),
        "expansion" | "core" | "draft_innovation" | "starter" | "box" | "promo" => {
            standard_expansion_template(ctx)
        }
        _ => standard_expansion_template(ctx),
    }
}

fn insert(legalities: &mut CardLegalities, format: LegalityFormat, status: LegalityStatus) {
    legalities.insert(format, status);
}

fn fill_all(legalities: &mut CardLegalities, status: LegalityStatus) {
    for format in LegalityFormat::ALL {
        insert(legalities, format, status);
    }
}

fn funny_or_nonconstructible_template() -> CardLegalities {
    let mut out = CardLegalities::new();
    fill_all(&mut out, LegalityStatus::NotLegal);
    out
}

/// Commander precon / spellbook product: legal in Commander-family formats.
fn commander_product_template(ctx: &InferenceContext<'_>) -> CardLegalities {
    let mut out = CardLegalities::new();
    fill_all(&mut out, LegalityStatus::NotLegal);
    for format in [
        LegalityFormat::Commander,
        LegalityFormat::DuelCommander,
        LegalityFormat::Oathbreaker,
        LegalityFormat::PauperCommander,
        LegalityFormat::Brawl,
        LegalityFormat::StandardBrawl,
        LegalityFormat::Historic,
        LegalityFormat::Timeless,
    ] {
        insert(&mut out, format, LegalityStatus::Legal);
    }
    if is_pauper_eligible(ctx.rarities) {
        insert(&mut out, LegalityFormat::Pauper, LegalityStatus::Legal);
        insert(
            &mut out,
            LegalityFormat::PauperCommander,
            LegalityStatus::Legal,
        );
    }
    out
}

/// Masters / reprint sets: eternal formats, not Standard.
fn masters_template(ctx: &InferenceContext<'_>) -> CardLegalities {
    let mut out = CardLegalities::new();
    fill_all(&mut out, LegalityStatus::NotLegal);
    for format in [
        LegalityFormat::Modern,
        LegalityFormat::Pioneer,
        LegalityFormat::Legacy,
        LegalityFormat::Vintage,
        LegalityFormat::Historic,
        LegalityFormat::Timeless,
        LegalityFormat::Commander,
        LegalityFormat::DuelCommander,
        LegalityFormat::Oathbreaker,
        LegalityFormat::Brawl,
        LegalityFormat::StandardBrawl,
    ] {
        insert(&mut out, format, LegalityStatus::Legal);
    }
    if is_pauper_eligible(ctx.rarities) {
        insert(&mut out, LegalityFormat::Pauper, LegalityStatus::Legal);
        insert(
            &mut out,
            LegalityFormat::PauperCommander,
            LegalityStatus::Legal,
        );
    }
    out
}

/// Standard-legal expansion / core set template (MSH, OTJ, etc.).
fn standard_expansion_template(ctx: &InferenceContext<'_>) -> CardLegalities {
    let mut out = CardLegalities::new();
    fill_all(&mut out, LegalityStatus::NotLegal);

    for format in [
        LegalityFormat::Standard,
        LegalityFormat::Modern,
        LegalityFormat::Pioneer,
        LegalityFormat::Legacy,
        LegalityFormat::Vintage,
        LegalityFormat::Historic,
        LegalityFormat::Brawl,
        LegalityFormat::StandardBrawl,
        LegalityFormat::Timeless,
        LegalityFormat::Commander,
        LegalityFormat::DuelCommander,
        LegalityFormat::Oathbreaker,
    ] {
        insert(&mut out, format, LegalityStatus::Legal);
    }

    if is_pauper_eligible(ctx.rarities) {
        insert(&mut out, LegalityFormat::Pauper, LegalityStatus::Legal);
        insert(
            &mut out,
            LegalityFormat::PauperCommander,
            LegalityStatus::Legal,
        );
    }

    // Planeswalkers with commander leadership skills remain commander-legal even
    // when the type line omits "Legendary".
    if is_commander_eligible(ctx) {
        insert(&mut out, LegalityFormat::Commander, LegalityStatus::Legal);
        insert(
            &mut out,
            LegalityFormat::DuelCommander,
            LegalityStatus::Legal,
        );
        insert(&mut out, LegalityFormat::Oathbreaker, LegalityStatus::Legal);
        insert(
            &mut out,
            LegalityFormat::PauperCommander,
            LegalityStatus::Legal,
        );
    }

    out
}

fn is_pauper_eligible(rarities: &BTreeSet<Rarity>) -> bool {
    rarities.contains(&Rarity::Common)
}

fn is_commander_eligible(ctx: &InferenceContext<'_>) -> bool {
    if let Some(skills) = ctx.leadership_skills {
        if skills.commander || skills.brawl || skills.oathbreaker {
            return true;
        }
    }
    ctx.type_line.is_some_and(|line| line.contains("Legendary"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::set_catalog::{ReleaseDate, SetCatalog, SetMeta, GATED_SETS_AS_OF_ENV};

    fn msh_catalog() -> SetCatalog {
        let mut catalog = SetCatalog::default();
        for (code, set_type) in [("MSH", "expansion"), ("MSC", "commander")] {
            catalog.insert_test_meta(SetMeta {
                code: code.into(),
                name: code.into(),
                release_date: ReleaseDate::parse("2026-06-26"),
                set_type: Some(set_type.into()),
                is_online_only: false,
                parent_code: None,
            });
        }
        catalog
    }

    fn ctx<'a>(
        printings: &'a [String],
        catalog: &'a SetCatalog,
        rarities: &'a BTreeSet<Rarity>,
    ) -> InferenceContext<'a> {
        InferenceContext {
            printings,
            catalog,
            leadership_skills: None,
            type_line: Some("Legendary Creature — Human Hero"),
            rarities,
        }
    }

    #[test]
    fn needs_inference_only_when_empty() {
        assert!(needs_inference(&CardLegalities::new()));
        let mut with_data = CardLegalities::new();
        with_data.insert(LegalityFormat::Standard, LegalityStatus::Legal);
        assert!(!needs_inference(&with_data));
    }

    #[test]
    fn msh_expansion_infers_standard_legal() {
        std::env::set_var(GATED_SETS_AS_OF_ENV, "2026-06-30");
        let catalog = msh_catalog();
        let printings = vec!["MSH".to_string()];
        let rarities = BTreeSet::from([Rarity::Rare]);
        let inferred = infer_missing_legalities(&ctx(&printings, &catalog, &rarities));
        assert_eq!(
            inferred.get(&LegalityFormat::Standard),
            Some(&LegalityStatus::Legal)
        );
        assert_eq!(
            inferred.get(&LegalityFormat::Modern),
            Some(&LegalityStatus::Legal)
        );
        assert_eq!(
            inferred.get(&LegalityFormat::Premodern),
            Some(&LegalityStatus::NotLegal)
        );
        std::env::remove_var(GATED_SETS_AS_OF_ENV);
    }

    #[test]
    fn msc_commander_precon_infers_commander_legal_not_standard() {
        std::env::set_var(GATED_SETS_AS_OF_ENV, "2026-06-30");
        let catalog = msh_catalog();
        let printings = vec!["MSC".to_string()];
        let rarities = BTreeSet::from([Rarity::Rare]);
        let inferred = infer_missing_legalities(&ctx(&printings, &catalog, &rarities));
        assert_eq!(
            inferred.get(&LegalityFormat::Commander),
            Some(&LegalityStatus::Legal)
        );
        assert_eq!(
            inferred.get(&LegalityFormat::Standard),
            Some(&LegalityStatus::NotLegal)
        );
        std::env::remove_var(GATED_SETS_AS_OF_ENV);
    }

    #[test]
    fn pauper_eligible_when_common_printing_exists() {
        std::env::set_var(GATED_SETS_AS_OF_ENV, "2026-06-30");
        let catalog = msh_catalog();
        let printings = vec!["MSH".to_string()];
        let rarities = BTreeSet::from([Rarity::Common, Rarity::Rare]);
        let inferred = infer_missing_legalities(&ctx(&printings, &catalog, &rarities));
        assert_eq!(
            inferred.get(&LegalityFormat::Pauper),
            Some(&LegalityStatus::Legal)
        );
        std::env::remove_var(GATED_SETS_AS_OF_ENV);
    }

    #[test]
    fn no_inference_when_printings_not_all_released() {
        std::env::set_var(GATED_SETS_AS_OF_ENV, "2026-06-01");
        let catalog = msh_catalog();
        let printings = vec!["MSH".to_string()];
        let rarities = BTreeSet::new();
        let inferred = infer_missing_legalities(&ctx(&printings, &catalog, &rarities));
        assert!(inferred.is_empty());
        std::env::remove_var(GATED_SETS_AS_OF_ENV);
    }
}
