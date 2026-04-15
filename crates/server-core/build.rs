use std::process::Command;

fn main() {
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout)
                    .ok()
                    .map(|s| s.trim().to_owned())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_owned());

    println!("cargo:rustc-env=PHASE_BUILD_COMMIT={commit}");

    // Rebuild when HEAD moves (new commit, branch switch, rebase).
    // The .git/ path is workspace-relative; crates/server-core/build.rs sits two levels deep.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    // After `git gc`, refs migrate into packed-refs. Watch that too so the
    // short-SHA stays current in long-running dev environments.
    println!("cargo:rerun-if-changed=../../.git/packed-refs");
}
