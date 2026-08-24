//! Support shared by both test binaries. Not a suite: nothing here asserts anything, and
//! `common/mod.rs` is a module both suites include, never a test binary of its own.

use std::io;
use std::path::Path;

/// The portable `symlink` both suites need, signature-compatible with the `std` calls it
/// replaces so no call site changes shape.
///
/// It exists so the suites COMPILE off Unix. `std::os::unix` is configured away on Windows, so
/// the seven `std::os::unix::fs::symlink` call sites were the last thing failing a
/// `cargo check --workspace --all-targets` on a Windows checkout — an error at every one of
/// them, before any test could report anything at all.
///
/// Compiling is not running. Windows creates symlinks only with Developer Mode on or from an
/// elevated shell, so the Tier-1 callers carry `#[cfg_attr(windows, ignore = …)]` and report as
/// IGNORED there — the same vocabulary this suite already uses for `spawns GNU timeout(1)`, and
/// the reason it uses it: a test that vanishes from the report is indistinguishable from one
/// that passed. (Every Tier-2 test is `#[ignore]`d unconditionally already, so those need
/// nothing added.)
#[cfg(unix)]
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Windows splits the one Unix call in two and fixes the flavour at CREATION time, so the
/// flavour is chosen from what `target` IS rather than from how the link is later read.
///
/// A target that does not exist gets the file flavour. That is the DANGLING link
/// `path_surface_is_realpath_containment` creates deliberately, and the arm only cares that it
/// fails to resolve — which neither flavour does when nothing is there.
#[cfg(windows)]
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
    let target = target.as_ref();
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}
