//! Expected quality of a random-creature pool, by mana value.
//!
//! `Effect::CreateTokenCopyFromPool` (the Momir's Madness emblem) copies a
//! creature card chosen at random from the whole loaded card database, filtered
//! by mana value (CR 202.3) and the effect's type filter. The only thing the
//! activating player controls is X, so the only thing worth knowing about the
//! pool is "how good is a random draw at this X?". This module answers that
//! from the same database, through the resolver's own eligibility predicate
//! (`face_is_eligible`), so the profile can never describe a pool the resolver
//! does not draw from.
//!
//! # The quality measure
//!
//! A face's quality is its combat body — [`creature_combat_value`] over its
//! printed power, toughness, and keywords, with the default keyword bonuses —
//! the same units `evaluate_creature_intrinsic` prices a creature already on
//! the battlefield in. That shared unit is what lets the Momir policy weigh
//! "a better pool at X+1" against "a mana creature tapped for a turn".
//!
//! Faces whose power or toughness is not a fixed printed number (`*`, X,
//! derived) are drawable, so they count toward whether a pool is empty, but
//! have no body to score and are left out of the mean rather than scored as 0.
//!
//! # Caching
//!
//! Building a profile walks every face in the database (tens of thousands), and
//! the policy asks for it on every activation and `{X}` candidate it scores.
//! The profile is a pure function of the database and the type filter, so it is
//! built once per (database, filter) pair and shared process-wide. Entries hold
//! a `Weak` to their database: a live `Weak` keeps the allocation's address
//! reserved, so a new database can never alias a cached one, and entries whose
//! database has been dropped are pruned on the next lookup.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use engine::database::card_db::CardDatabase;
use engine::game::effects::create_token_copy_from_pool::face_is_eligible;
use engine::types::ability::{Comparator, PtValue, TargetFilter};
use engine::types::card::CardFace;
use engine::types::game_state::GameState;

use crate::eval::{creature_combat_value, KeywordBonuses};

/// Draw statistics for the faces at one mana value.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Bucket {
    /// Faces the resolver could draw at this mana value.
    drawable: u32,
    /// Drawable faces with a fixed printed body — the ones the mean covers.
    scored: u32,
    /// Sum of the scored faces' combat bodies.
    body_sum: f64,
}

/// Per-mana-value draw statistics for one random-creature pool.
#[derive(Debug, Default, PartialEq)]
pub(super) struct PoolProfile {
    by_mana_value: BTreeMap<u32, Bucket>,
}

impl PoolProfile {
    /// Profile every face `db` holds that the resolver would accept under
    /// `type_filter` at the face's own mana value.
    fn build(db: &CardDatabase, type_filter: &TargetFilter) -> Self {
        let bonuses = KeywordBonuses::default();
        let mut by_mana_value: BTreeMap<u32, Bucket> = BTreeMap::new();
        for face in db.faces_in_scan_order() {
            let mana_value = face.mana_cost.mana_value();
            // CR 202.3: every eligibility test other than the mana-value bound
            // is independent of X, so "eligible at its own mana value" is
            // exactly "eligible for any bound that admits its mana value".
            if !face_is_eligible(face, Comparator::EQ, mana_value as i32, type_filter) {
                continue;
            }
            let bucket = by_mana_value.entry(mana_value).or_default();
            bucket.drawable += 1;
            if let Some(body) = face_body(face, &bonuses) {
                bucket.scored += 1;
                bucket.body_sum += body;
            }
        }
        Self { by_mana_value }
    }

    /// Mean combat body of a random draw whose mana value satisfies `cmp`
    /// against `bound` — `None` when nothing is drawable there (CR 609.3: the
    /// activation would create no token) or nothing drawable has a fixed body
    /// to score.
    pub(super) fn expected_body(&self, cmp: Comparator, bound: u32) -> Option<f64> {
        let (scored, body_sum) = self
            .by_mana_value
            .iter()
            .filter(|(mana_value, _)| cmp.evaluate(**mana_value as i32, bound as i32))
            .fold((0u32, 0.0f64), |(scored, sum), (_, bucket)| {
                (scored + bucket.scored, sum + bucket.body_sum)
            });
        (scored > 0).then(|| body_sum / f64::from(scored))
    }
}

/// A face's combat body, or `None` when its power or toughness is not a fixed
/// printed number.
fn face_body(face: &CardFace, bonuses: &KeywordBonuses) -> Option<f64> {
    let (Some(PtValue::Fixed(power)), Some(PtValue::Fixed(toughness))) =
        (&face.power, &face.toughness)
    else {
        return None;
    };
    Some(creature_combat_value(
        *power,
        *toughness,
        |keyword| face.keywords.contains(keyword),
        bonuses,
    ))
}

struct CacheEntry {
    db: Weak<CardDatabase>,
    type_filter: TargetFilter,
    profile: Arc<PoolProfile>,
}

static PROFILES: Mutex<Vec<CacheEntry>> = Mutex::new(Vec::new());

/// The pool profile for draws from `state`'s card database under
/// `type_filter`, or `None` when no database is installed.
pub(super) fn pool_profile(
    state: &GameState,
    type_filter: &TargetFilter,
) -> Option<Arc<PoolProfile>> {
    let db = state.card_db.as_ref()?.arc();
    // A poisoned lock only means another thread panicked mid-lookup; the cache
    // holds no invariant that a partial update could break (entries are pushed
    // whole), so keep using it.
    let mut cache = PROFILES.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|entry| entry.db.strong_count() > 0);
    if let Some(entry) = cache.iter().find(|entry| {
        std::ptr::eq(entry.db.as_ptr(), Arc::as_ptr(db)) && entry.type_filter == *type_filter
    }) {
        return Some(Arc::clone(&entry.profile));
    }
    // Built under the lock so concurrent searches never walk the database twice.
    let profile = Arc::new(PoolProfile::build(db, type_filter));
    cache.push(CacheEntry {
        db: Arc::downgrade(db),
        type_filter: type_filter.clone(),
        profile: Arc::clone(&profile),
    });
    Some(profile)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::format::FormatConfig;
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use serde_json::{Map, Value};

    /// A creature face costing `mana_value` generic with the given body.
    pub(in crate::policies) fn creature_face(
        name: &str,
        mana_value: u32,
        power: Option<i32>,
        toughness: i32,
    ) -> CardFace {
        CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![],
                generic: mana_value,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Beast".to_string()],
            },
            power: Some(power.map_or(PtValue::Variable("*".to_string()), PtValue::Fixed)),
            toughness: Some(PtValue::Fixed(toughness)),
            ..Default::default()
        }
    }

    /// Install a card database holding exactly `faces` on `state`.
    pub(in crate::policies) fn install_db(state: &mut GameState, faces: &[CardFace]) {
        let export: Map<String, Value> = faces
            .iter()
            .map(|face| {
                (
                    face.name.to_lowercase(),
                    serde_json::to_value(face).expect("CardFace serializes"),
                )
            })
            .collect();
        let db = CardDatabase::from_json_str(&Value::Object(export).to_string())
            .expect("synthetic export parses");
        engine::game::install_card_db(state, Arc::new(db));
    }

    fn state_with(faces: &[CardFace]) -> GameState {
        let mut state = GameState::new(FormatConfig::momir(), 2, 42);
        install_db(&mut state, faces);
        state
    }

    #[test]
    fn no_database_means_no_profile() {
        let state = GameState::new(FormatConfig::momir(), 2, 42);
        assert!(pool_profile(&state, &TargetFilter::Any).is_none());
    }

    /// The mean covers exactly the faces drawable at that mana value.
    #[test]
    fn expected_body_is_the_mean_body_at_that_mana_value() {
        let state = state_with(&[
            creature_face("Two A", 2, Some(2), 2),
            creature_face("Two B", 2, Some(4), 4),
            creature_face("Three", 3, Some(3), 3),
        ]);
        let profile = pool_profile(&state, &TargetFilter::Any).unwrap();
        // (2*1.5+2 + 4*1.5+4) / 2 = 7.5
        assert_eq!(profile.expected_body(Comparator::EQ, 2), Some(7.5));
        assert_eq!(profile.expected_body(Comparator::EQ, 3), Some(7.5));
        // CR 609.3: an empty mana value has no draw to score.
        assert_eq!(profile.expected_body(Comparator::EQ, 4), None);
        // An "N or less" comparator pools every admitted mana value.
        assert_eq!(profile.expected_body(Comparator::LE, 3), Some(7.5));
    }

    /// A `*` body is drawable but unscored — it neither drags the mean to 0 nor
    /// makes an otherwise-empty pool look scoreable.
    #[test]
    fn variable_bodies_are_left_out_of_the_mean() {
        let state = state_with(&[
            creature_face("Fixed", 5, Some(4), 4),
            creature_face("Star", 5, None, 4),
            creature_face("Only Star", 6, None, 6),
        ]);
        let profile = pool_profile(&state, &TargetFilter::Any).unwrap();
        assert_eq!(profile.expected_body(Comparator::EQ, 5), Some(10.0));
        assert_eq!(profile.expected_body(Comparator::EQ, 6), None);
    }

    /// Keywords count, through the shared combat-value authority.
    #[test]
    fn keywords_raise_the_body() {
        let mut flier = creature_face("Flier", 4, Some(3), 3);
        flier.keywords.push(Keyword::Flying);
        let ground = creature_face("Ground", 5, Some(3), 3);
        let state = state_with(&[flier, ground]);
        let profile = pool_profile(&state, &TargetFilter::Any).unwrap();
        assert!(
            profile.expected_body(Comparator::EQ, 4).unwrap()
                > profile.expected_body(Comparator::EQ, 5).unwrap()
        );
    }

    /// Non-creature faces are not in the pool — the resolver's own predicate
    /// decides, so the profile can never describe a pool it does not draw from.
    #[test]
    fn non_creature_faces_are_not_drawable() {
        let mut artifact = creature_face("Rock", 3, Some(9), 9);
        artifact.card_type.core_types = vec![CoreType::Artifact];
        let state = state_with(&[artifact]);
        let profile = pool_profile(&state, &TargetFilter::Any).unwrap();
        assert_eq!(profile.expected_body(Comparator::EQ, 3), None);
    }

    /// One profile per database: a second lookup shares the first build, and a
    /// different database gets its own.
    #[test]
    fn profiles_are_cached_per_database() {
        let state = state_with(&[creature_face("A", 2, Some(2), 2)]);
        let first = pool_profile(&state, &TargetFilter::Any).unwrap();
        let again = pool_profile(&state.clone(), &TargetFilter::Any).unwrap();
        assert!(Arc::ptr_eq(&first, &again));

        let other = state_with(&[creature_face("B", 2, Some(9), 9)]);
        let other_profile = pool_profile(&other, &TargetFilter::Any).unwrap();
        assert!(!Arc::ptr_eq(&first, &other_profile));
        assert_ne!(
            first.expected_body(Comparator::EQ, 2),
            other_profile.expected_body(Comparator::EQ, 2)
        );
    }
}
