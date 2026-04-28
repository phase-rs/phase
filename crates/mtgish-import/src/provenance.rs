//! Provenance breadcrumb tracker for mtgish-import (Phase 13).
//!
//! Every successfully-converted item produced by the converter (each
//! `AbilityDefinition`, `TriggerDefinition`, `StaticDefinition`,
//! `ReplacementDefinition`, and `Keyword`) carries a side-channel
//! breadcrumb keyed by `(card_name, face_idx, slot, slot_idx)`. This is
//! a build-time artifact only: it does **not** live on `EngineFaceStub`
//! or `CardFace`. The accumulated map is written to a parallel JSON file
//! (typically `data/mtgish-import-provenance.json`) alongside the
//! existing diagnostic report.
//!
//! Path shape (mirrors `report.rs` keys):
//!
//! ```text
//! mtgish:Card/Rules[<idx>]/Rule::<TopVariant>
//! ```
//!
//! The output JSON additionally carries a `card_data_hash` stamp that
//! Phase 14 will populate with the SHA-256 of the canonical-form
//! `card-data.json`. For now, the placeholder `"unstamped"` is written.
//!
//! Provenance is recorded only for **successful** conversions. Failed
//! rules already surface in `ImportReport` and must not appear here.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// Which slot on the `EngineFaceStub` an item landed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ProvenanceSlot {
    Ability,
    Trigger,
    Static,
    Replacement,
    Keyword,
}

/// One produced item's breadcrumb.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProvenanceEntry {
    pub face_idx: usize,
    pub slot: ProvenanceSlot,
    pub slot_idx: usize,
    pub path: String,
}

/// Per-card accumulator. One per converted card.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CardProvenance {
    pub entries: Vec<ProvenanceEntry>,
}

impl CardProvenance {
    pub fn record(&mut self, entry: ProvenanceEntry) {
        self.entries.push(entry);
    }
}

/// Corpus-wide accumulator. Card name → per-card breadcrumbs.
#[derive(Debug, Default)]
pub struct ProvenanceTracker {
    map: BTreeMap<String, CardProvenance>,
}

impl ProvenanceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mutable access to the per-card slot — convenient when a converter
    /// driver wants to record several entries against the same card.
    pub fn entry_for(&mut self, card: &str) -> &mut CardProvenance {
        self.map.entry(card.to_string()).or_default()
    }

    /// Record a single entry for a card.
    pub fn record(&mut self, card: &str, entry: ProvenanceEntry) {
        self.entry_for(card).record(entry);
    }

    pub fn cards(&self) -> usize {
        self.map.len()
    }

    pub fn total_entries(&self) -> usize {
        self.map.values().map(|c| c.entries.len()).sum()
    }

    /// Serialize the tracker to disk.
    ///
    /// `card_data_hash` stamps the output so a downstream consumer can
    /// detect drift between the provenance file and the card-data file
    /// it was generated against. Phase 14 wires the real SHA-256; for
    /// now callers pass `"unstamped"`.
    // TODO(phase-14): wire actual card-data.json SHA-256 hash through
    // the binary so the provenance file can be coherence-checked.
    pub fn write_to(&self, path: &Path, card_data_hash: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let body = serde_json::json!({
            "card_data_hash": card_data_hash,
            "provenance": self.map.iter().map(|(name, prov)| {
                (name.clone(), &prov.entries)
            }).collect::<BTreeMap<_, _>>(),
        });
        let json = serde_json::to_string_pretty(&body).map_err(io::Error::other)?;
        std::fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_one_entry() {
        let mut tracker = ProvenanceTracker::new();
        tracker.record(
            "Lightning Bolt",
            ProvenanceEntry {
                face_idx: 0,
                slot: ProvenanceSlot::Ability,
                slot_idx: 0,
                path: "main/Rules[0]/Rule::SpellActions".into(),
            },
        );
        assert_eq!(tracker.cards(), 1);
        assert_eq!(tracker.total_entries(), 1);
        let card = tracker.entry_for("Lightning Bolt");
        assert_eq!(card.entries.len(), 1);
        assert_eq!(card.entries[0].face_idx, 0);
        assert_eq!(card.entries[0].slot, ProvenanceSlot::Ability);
    }

    #[test]
    fn multiple_entries_same_card() {
        let mut tracker = ProvenanceTracker::new();
        tracker.record(
            "Soul Warden",
            ProvenanceEntry {
                face_idx: 0,
                slot: ProvenanceSlot::Trigger,
                slot_idx: 0,
                path: "main/Rules[0]/Rule::TriggerA".into(),
            },
        );
        tracker.record(
            "Soul Warden",
            ProvenanceEntry {
                face_idx: 0,
                slot: ProvenanceSlot::Keyword,
                slot_idx: 0,
                path: "main/Rules[1]/Rule::Keyword".into(),
            },
        );
        assert_eq!(tracker.cards(), 1);
        assert_eq!(tracker.total_entries(), 2);
    }

    #[test]
    fn serializes_with_hash_stamp() {
        let mut tracker = ProvenanceTracker::new();
        tracker.record(
            "Test Card",
            ProvenanceEntry {
                face_idx: 0,
                slot: ProvenanceSlot::Static,
                slot_idx: 0,
                path: "main/Rules[0]/Rule::PermanentLayerEffect".into(),
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provenance.json");
        tracker.write_to(&path, "deadbeef").unwrap();
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(body["card_data_hash"], "deadbeef");
        assert!(body["provenance"]["Test Card"].is_array());
        assert_eq!(body["provenance"]["Test Card"][0]["slot"], "Static");
    }
}
