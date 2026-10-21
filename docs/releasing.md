# Releasing

[.github/workflows/build.yml](../.github/workflows/build.yml) does two
separate things, and the difference between them matters more here than in
most projects: it **checks** Geode on three platforms, and on a tag it
**builds installers** for three platforms. Neither is the same as having run
Geode there.

## What the workflow runs

**`check`** — on every push to `main`, every pull request, and every tag.
Runs on `ubuntu-22.04`, `windows-2022` and `macos-15`, and on each one runs
the whole gate a change is held to locally:

```
npm ci · npm run typecheck · npm test · npm run build
cargo test --lib · cargo clippy --all-targets -- -D warnings
```

plus the three files in `src-tauri/tests/` that need no network —
`backup_roundtrip` (5 tests), `curseforge_fingerprints` (24) and
`header_wire_format` (1). A warning is a failure, in CI as on a desk.

What this does *not* cover: the nine `live_*` files hold 48 `#[ignore]`d
tests that reach real upstream APIs, and CI runs none of them. Unit tests
stay offline, so the offline ones are all any platform gets here.

**`bundle`** — only on a tag matching `v*`, and only once `check` has passed
on all three. Builds the installers and attaches them to a **draft** GitHub
release:

| Runner | `bundle.targets` | Out |
|---|---|---|
| `ubuntu-22.04` | `appimage`, `deb`, `rpm` | the three Linux packages already built by hand |
| `windows-2022` | `nsis` | one x64 `-setup.exe` installer |
| `macos-15` | `app`, `dmg` | one universal dmg, arm64 and x86_64 in one file |

macOS is built with `--target universal-apple-darwin` rather than one job per
architecture because a player should not have to know which kind of Mac they
have. It costs a second compile of every crate; macOS runner minutes are
billed at ten times the rate, so one universal job costs roughly what two
single-arch jobs would.

The release is a draft on purpose. Somebody signs off before three bundles,
two of which nobody has launched, go out under a version number.

## What a green run proves

| | Compiles | Offline tests | Installer built | Ever launched | Claims `geode://` through |
|---|---|---|---|---|---|
| Linux | yes | yes | yes | yes | the desktop entry, plus a runtime registration on each start |
| Windows | yes | yes | yes | **no** | `Software\Classes\geode`, written by the NSIS installer |
| macOS | yes | yes | yes | **no** | `CFBundleURLTypes` in the .app's `Info.plist` |

"Built by CI" is not "works on Windows". Nothing in this repository has
opened an `Info.plist`, read a registry key, launched the app, signed into
Microsoft, started a JVM or clicked a `geode://` link on either platform. The
specific things most likely to be wrong first:

- **A downloaded Java runtime on macOS.** Geode fetches a JRE and runs it.
  macOS quarantines what a browser or an app downloads, and an unsigned
  binary that is launched from quarantine is killed. Nothing here has tried
  it.
- **Gatekeeper and SmartScreen.** Unsigned, macOS tells the player the app
  cannot be verified and they have to allow it in Privacy & Security;
  Windows shows SmartScreen's warning. Signing fixes the first and only
  improves the second over time, as reputation accrues to a certificate.
- **Paths.** `paths.rs` is the only place that knows the layout, which is the
  reason to expect this to hold — but `~/Library/Application Support` and
  `%APPDATA%` have never been written to.
- **The pack host.** `share/host.rs` clears `IPV6_V6ONLY` by hand because
  Windows, unlike Linux, turns it on, and a listener left v6-only refuses
  every IPv4 player. That line has never run on Windows.

## Secrets the owner has to create

Every one of these is optional and none is in the repository. Absent, the
build still succeeds: the Windows installer is unsigned, and the macOS app is
signed ad-hoc and not notarised. Set them under Settings → Secrets and
variables → Actions.

| Secret | What it is | How to make it |
|---|---|---|
| `APPLE_CERTIFICATE` | A Developer ID Application certificate, as base64 | Create the certificate in your Apple Developer account, install it, export the private key from Keychain Access as a `.p12`, then `openssl base64 -A -in certificate.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | The password used on that export | You choose it when exporting |
| `APPLE_SIGNING_IDENTITY` | The identity's full name, e.g. `Developer ID Application: Name (TEAMID)` | `security find-identity -v -p codesigning` on a Mac that has it |
| `APPLE_ID` | The Apple account email, for notarisation | — |
| `APPLE_PASSWORD` | An **app-specific** password for that account, not the account password | appleid.apple.com → Sign-In and Security → App-Specific Passwords |
| `APPLE_TEAM_ID` | The team identifier | Apple Developer → Membership |

`GITHUB_TOKEN` is issued by Actions itself; the `bundle` job asks for
`contents: write` so it can create the draft release. If a run fails with
"Resource not accessible by integration", the repository's Actions
permissions are set to read-only.

Without `APPLE_SIGNING_IDENTITY` the workflow passes `-`, the ad-hoc
pseudo-identity. That is not a shortcut around signing: it is what an Apple
Silicon Mac requires before it will run a downloaded app at all, and without
it the dmg is reported as damaged rather than merely unverified.
Notarisation needs a real Developer ID certificate and a paid account; a free
Apple account cannot notarise.

`APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID` are handed to the build by
the certificate-import step rather than set beside `APPLE_SIGNING_IDENTITY`,
and only once all three hold something. The Tauri CLI asks whether those
variables are *set*, not whether they are useful: absent, it skips
notarisation; empty, it runs `xcrun notarytool --apple-id ""` and the build
fails. GitHub gives a missing secret as the empty string, so the three cannot
be passed through unconditionally.

### Windows signing is deliberately not wired

Tauri signs Windows bundles from `bundle.windows.certificateThumbprint`,
which names a certificate in the machine's own certificate store. Putting a
thumbprint in the config would break every build on every machine that does
not have that certificate — including the CI runners, until the step that
imports it exists. When there is a certificate to sign with, it takes two
things: a step on the Windows runner that decodes a base64 `.pfx` secret and
imports it with `certutil`, and `certificateThumbprint`, `digestAlgorithm`
and `timestampUrl` in `src-tauri/tauri.windows.conf.json`. Tauri's own
[Windows signing guide](https://v2.tauri.app/distribute/sign/windows/) has
the exact commands, including the Azure Trusted Signing route, which needs no
certificate on the runner at all.

### The updater is not wired, so it needs no key

There is no `tauri-plugin-updater` in `Cargo.toml`, no `plugins.updater`
block, and no `bundle.createUpdaterArtifacts`. Nothing signs an update,
so **do not create `TAURI_SIGNING_PRIVATE_KEY` yet** — a secret that nothing
reads is a secret nobody audits. When the updater lands it needs
`tauri signer generate`, a private key held only as that secret with
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, and its public half in the config.

## No API key is needed to build

The CurseForge key and the Microsoft client id are per-developer and
per-application, read through `option_env!`, and absent they are simply
absent: CurseForge search is off until the user pastes their own key, and
Microsoft sign-in is off until a build is compiled with a client id. CI bakes
neither, and it must not — handing someone a binary containing a CurseForge
key is exactly what CurseForge's terms forbid. A private build that does bake
them reads a gitignored `.env`; see [`.env.example`](../.env.example).

## Runner images are pinned, and will need bumping

`ubuntu-22.04`, `windows-2022` and `macos-15` are named rather than
`-latest`, because what a bundle is built against decides which machines can
install it: the oldest glibc GitHub still offers, a VC runtime most Windows
machines already have, and an Xcode a release older than the newest, since a
newer one raises the oldest macOS a build can target. GitHub retires runner
images on its own schedule. When one of these goes, bumping it moves the
floor Geode supports, so say in the release notes that it moved.

## Cutting a release

1. Bump the version in **three** places, which nothing checks agree:
   `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
   The installers are named from the one in `tauri.conf.json`, so a tag that
   disagrees with it produces assets labelled with the old version.
2. Commit, then tag `vX.Y.Z` and push the tag.
3. `check` runs on three platforms, then `bundle` builds on three and opens a
   draft release.
4. Download the Linux package and run it. Say, in the release notes, which
   bundles were launched and which were not.
5. Publish the draft.

Nothing in this repository parses the workflow file — GitHub is the first
thing that reads it, and a syntax error there surfaces as a run that never
starts. Changing it is worth a push to a branch before a tag depends on it.
