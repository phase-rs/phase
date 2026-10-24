/**
 * The commit hash shown beside the version in the UI. Only the preview
 * deployment (deploy.yml → preview.phase-rs.dev) surfaces it; release, dev and
 * self-hosted builds show the bare version. Diagnostic reports still embed
 * `__BUILD_HASH__` directly.
 */
export function displayedBuildHash(): string | null {
  return __IS_PREVIEW_BUILD__ ? __BUILD_HASH__ : null;
}
