//! Shared combo-table loading for native AI harness binaries.

use std::path::Path;

use engine::database::ComboTable;

/// Loads the requested combo artifact, or returns an explicitly unmeasured
/// table when no path was supplied.
pub fn load(path: Option<&Path>) -> Result<ComboTable, String> {
    let Some(path) = path else {
        return Ok(ComboTable::default());
    };

    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to load {}: {error}", path.display()))?;
    ComboTable::from_json_str(&raw)
        .map_err(|error| format!("failed to load {}: {error}", path.display()))
}

/// Prints the measurement identity used by a harness run.
pub fn print_provenance(table: &ComboTable) {
    match table.provenance() {
        Some(provenance) => eprintln!(
            "combo table: {} (version {}), {} entries",
            provenance.snapshot_date,
            provenance.table_version,
            table.len()
        ),
        None => eprintln!("combo table: unmeasured"),
    }
}
