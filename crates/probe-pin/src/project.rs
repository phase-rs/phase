//! §7 steps 4 and 11: instrument versions, and the ast-grep projections.
//!
//! Recording rule, applied once: an instrument is recorded IFF it produced a number in the
//! block. `rustc` always did — every verdict came through libtest's nightly-only JSON surface.
//! `ast-grep` only did when the manifest declares at least one projection.

use std::path::Path;
use std::process::Command;

use crate::manifest::Projection;
use crate::{Abort, Instrument};

/// `PROBE_PIN_ASTGREP` serves non-PATH installs and is what makes every projection row
/// testable with no external install. There is no `PROBE_PIN_RUSTC` twin: `rustc` is on PATH
/// wherever cargo built the target, so that knob would have zero users.
pub fn astgrep_tool() -> String {
    std::env::var("PROBE_PIN_ASTGREP").unwrap_or_else(|_| "ast-grep".to_string())
}

fn version_of(program: &str) -> Result<String, String> {
    let out = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The program each instrument is read from. `rustc` is on PATH wherever cargo built the
/// target; ast-grep's is overridable.
pub fn program_for(instrument: Instrument) -> String {
    match instrument {
        Instrument::Toolchain => "rustc".to_string(),
        Instrument::AstGrep => astgrep_tool(),
    }
}

/// The tool is a parameter, not a hidden env read, so a test can drive a fake one without
/// mutating process-wide state that its parallel siblings share.
pub fn instrument_version(instrument: Instrument, program: &str) -> Result<String, Abort> {
    version_of(program).map_err(|stderr| match instrument {
        Instrument::Toolchain => Abort::InstrumentUnavailable { instrument, stderr },
        Instrument::AstGrep => Abort::ProjectionToolMissing {
            tool: program.to_string(),
            stderr,
        },
    })
}

/// One match per line (`--json=stream`), so the count is the line count.
pub fn count(projection: &Projection, root: &Path, tool: &str) -> Result<usize, Abort> {
    let out = Command::new(tool)
        .current_dir(root)
        .args(["run", "--pattern", &projection.pattern, "--json=stream"])
        .args(&projection.paths)
        .output()
        .map_err(|e| Abort::ProjectionToolMissing {
            tool: tool.to_string(),
            stderr: e.to_string(),
        })?;
    if !out.status.success() {
        return Err(Abort::ProjectionToolMissing {
            tool: tool.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count())
}

pub fn sentence(projection: &Projection, count: usize) -> String {
    projection.sentence.replace("{count}", &count.to_string())
}
