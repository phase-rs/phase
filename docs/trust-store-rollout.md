# Bundled TLS trust-store update (#10182)

Tracking issue: [#10182](https://github.com/phase-rs/phase/issues/10182).
[RUSTSEC-2026-0381](https://rustsec.org/advisories/RUSTSEC-2026-0381.html)
requires `webpki-roots` 1.0.10 or newer: it removes emSign ECC Root CA - G3
following compromise of intermediate CA keys. This is an upstream trust-store
incident; there is no evidence that Phase itself was compromised.

## Scope

- `phase-server` uses reqwest 0.12 with `rustls-tls-webpki-roots`. Its bundled
  roots come from the root `Cargo.lock`. This includes standalone servers,
  Docker images, and native server sidecars downloaded by the desktop shell.
- The Tauri shell has a separate lockfile. Its reqwest 0.13 `rustls` feature
  uses the OS verifier on native targets ([reqwest manifest](https://github.com/seanmonstar/reqwest/blob/v0.13.4/Cargo.toml)).
  The locked `webpki-root-certs` dependency belongs to the verifier's
  [wasm target path](https://github.com/rustls/rustls-platform-verifier/blob/v/0.7.0/rustls-platform-verifier/Cargo.toml);
  its presence does not
  establish that native desktop HTTPS uses that bundle. Updating it is
  dependency hygiene, separate from rebuilding the affected server sidecar.
- OS trust updates remain the responsibility of the operating system. Do not
  disable certificate verification or add the removed root to restore access
  to an endpoint still chaining through it.

## Release and deployment checklist

Merging a lockfile change does not change trust roots embedded in existing
binaries. Leave #10182 open until rollout evidence is recorded there. The
following steps are pending when this document is introduced:

- [ ] Record the merged commit and verify `cargo tree --locked -p phase-server
  -i webpki-roots` resolves 1.0.10 or newer, without an older independent bundle.
- [ ] Finish the normal CI and staging deployment for that commit. Confirm
  `deploy.yml` rebuilt both server image architectures and `preview-server.yml`
  published signed sidecars for all four supported triples. Record the workflow
  runs, image digest, commit, and new preview fingerprint in #10182.
- [ ] Confirm `preview-server.json` selects that fingerprint and a desktop
  preview session fetches/starts the matching sidecar. `engine-fingerprint.sh`
  includes the root lockfile, so a trust-store-only change gets a new key and
  cannot reuse the already-published vulnerable binary under the old key.
- [ ] Create the next normal workspace release containing the fix using the
  documented `cargo release-local` procedure. Confirm `release.yml` publishes
  the rebuilt Linux images, four signed server artifacts, and versioned
  `release-server-v<version>.json` manifest. Record the release and image digest.
- [ ] Smoke-test HTTPS startup data downloads from an empty `PHASE_DATA_DIR`
  using the rebuilt server and its signed release/preview manifest. Confirm a
  desktop session downloads and starts the updated server sidecar and can
  download data over HTTPS. Verify signatures and hashes remain enforced;
  a successful HTTP-only local test does not check the refreshed trust store.
- [ ] Roll out the rebuilt server images to running deployments and restart
  them. Tell self-hosters to upgrade their binaries/images, and desktop users
  to select the fixed engine release/channel. Existing processes and cached
  old sidecars keep their original embedded roots until replaced.
- [ ] Record smoke-test results and deployed versions/digests in #10182 before
  closing it. Retained previous preview entries and older release assets still
  contain old roots; do not use them as a security rollback. If rollback is
  needed, rebuild the desired source with the patched trust store.

The shell's separate lockfile update alone does not require a shell release
to remediate the server's embedded bundle. A shell release containing that
update can follow its normal schedule; the server and sidecar rollout above
is the security completion boundary.
