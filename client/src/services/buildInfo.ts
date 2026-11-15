/**
 * The commit hash shown beside the version in the UI. Only the preview
 * deployment (deploy.yml → preview.phase-rs.dev) surfaces it; release, dev and
 * self-hosted builds show the bare version. Diagnostic reports still embed
 * `__BUILD_HASH__` directly.
 */
export function displayedBuildHash(): string | null {
  if (!__IS_PREVIEW_BUILD__ || typeof window === "undefined") return null;
  return window.location.origin === new URL(__PREVIEW_SITE_URL__).origin
    ? __BUILD_HASH__
    : null;
}
