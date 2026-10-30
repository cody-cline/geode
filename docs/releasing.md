# Releasing

[.github/workflows/build.yml](../.github/workflows/build.yml) does two
separate things, and the difference between them matters more here than in
most projects: it **checks** Geode on three platforms, and when it is asked
to, it **builds installers** for three platforms. Neither is the same as
having run Geode there.

## What the workflow runs

**`check`** — on every push to `main`, every pull request, and every manual
run, on `ubuntu-22.04`, `windows-2022` and `macos-15`. Each of them runs the
whole gate a change is held to locally:

```
npm ci · npm run typecheck · npm test · npm run build
cargo test --lib · cargo clippy --all-targets -- -D warnings
```

plus the three files in `src-tauri/tests/` that need no network —
`backup_roundtrip` (5 tests), `curseforge_fingerprints` (24) and
`header_wire_format` (1). A warning is a failure, in CI as on a desk.

This cost nothing, and briefly cost a great deal. GitHub bills a Windows
minute at two and a macOS minute at ten, so while the repository was private
all three on every push came to about 88 billable minutes a time — 23 pushes
against the 2000 a month a private repository gets, and it emptied the month
in eight days. The workflow was cut back to Linux on a push for exactly as
long as that was true. Going public ended it: standard runners are free and
unmetered on a public repository, macOS included, and only *larger* runners
are still billed. All three run on everything again, because a check that
only runs on demand is a check nobody runs.

Everything above compiles the desktop app, so Linux runs one step more:

```
cargo clippy --all-targets --no-default-features -- -D warnings
```

That is the only thing here that compiles `geoded` and `src/daemon/` — the
desktop build is handed several Cargo features for free by Tauri, so a
dependency the host needs spelled out can go missing while every step above
stays green. Linux alone, because a missing Cargo feature is not a platform
difference and the host is a Linux container.
`daemon::tests::ci_compiles_the_headless_build_and_not_only_the_app` is what
keeps this file and that step in step.

What this does *not* cover: the nine `live_*` files hold 48 `#[ignore]`d
tests that reach real upstream APIs, and CI runs none of them. Unit tests
stay offline, so the offline ones are all any platform gets here.

**`bundle`** — only when the workflow is run by hand with **release** ticked,
and only once `check` has passed on all three. Pushing a tag builds nothing,
on purpose: work lands on `main` in small commits for weeks, and only the
owner knows which run of them is worth shipping. The **tag** box is what
names the git tag, the draft release and the version an installed Geode
compares its own against, and `bundle` refuses before it builds anything if
it was left empty — a release dispatched from `main` would otherwise be
published as "Geode main". Builds the installers and attaches them to a
**draft** GitHub release:

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

Updating in place is a sixth column nobody can fill in yet: it is wired on all
three (see [the updater](#the-updater-and-the-keypair-only-you-can-make)) and
has been performed on none, including Linux, because there is no signed release
to update from. Being *told* a newer version exists is a separate claim with a
separate answer: it needs no key and no signature, so it works in any build,
and it has been seen on none of the three either, because there is as yet no
release to be told about.

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
build still succeeds: the Windows installer is unsigned, the macOS app is
signed ad-hoc and not notarised, and the release carries no update manifest.
Set them under Settings → Secrets and variables → Actions. The three the
updater needs are below, under their own heading, because they are the ones
with a consequence you cannot undo.

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

### The updater, and the keypair only you can make

Geode can replace itself with a newer build from these releases, and it
installs one only if it was signed with your key. Nothing in this repository
holds either half of that key, and nothing here generated one — a keypair in
a repository is a keypair anybody can sign with, and a convincing placeholder
would be worse than none, because it would look real.

Until you make one, **installing** is off end to end and says so rather than
pretending. There is no `createUpdaterArtifacts` in the committed config,
`plugins > updater > pubkey` is deliberately the empty string, and
`GEODE_UPDATER_PUBKEY` is unset so `lib.rs` registers no updater plugin at
all. A release cut today carries no manifest, and no Geode goes looking for
one.

**Telling somebody is not installing, and a build with no key still tells
them.** Reading the public releases API needs no key, no token and no
signature:

```
https://api.github.com/repos/cody-cline/geode/releases/latest
```

So such a build asks that, compares the tag against its own version through
the same `update::offer` the signed path uses, and Settings → About says
*"Geode 0.2.0 is out, and this build cannot install it"* beside a button that
opens the releases page. It downloads nothing, writes nothing and restarts
nothing: the person takes the installer and runs it the way they ran the last
one. Which mode a build is in is one `match` in `check_for_update` with no way
through both arms, so the two never offer at once, and
`nothing_is_downloaded_or_installed_without_the_key_that_verifies_it` holds
`install_update` to demanding a key before it reaches a download, a write or a
restart.

Everything else is shared on purpose. One comparison, so a downgrade, a
version that will not parse and the version already running are refused
identically in both modes; one stored `skippedUpdate`, so a release skipped in
either stays skipped in the other; one "not now", which lasts the sitting.
`Release::version` takes the `v` off the tag before the comparison, because
releases are tagged `vX.Y.Z` while the signed manifest's `version` field is
not, and `offer` reads no leading `v`.

A lookup that fails is **not** read as "nothing newer". The API is
rate-limited by IP, answers 404 until the first release exists, and is a host
on the internet like any other, so a failure leaves `check_for_update` as an
error and Settings says *"Could not check for a newer Geode"* with the reason
and two ways on. "Up to date" is a claim, and a failed lookup does not support
it.

**Switching it on, once.**

1. Generate the keypair on your own machine. It writes two files and asks for
   a password; give it one.

   ```bash
   npm run tauri -- signer generate -w ~/.tauri/geode.key
   ```

   `~/.tauri/geode.key` is the private half, `~/.tauri/geode.key.pub` the
   public half. Back the private half up somewhere you will still have it in
   two years. **Lose it and nothing you have already shipped can ever be
   updated again**: every installed Geode trusts exactly the key it was
   compiled with, there is no revocation and no second chance, and the only
   way out is a new key and everybody reinstalling by hand.

2. Add three repository secrets, under Settings → Secrets and variables →
   Actions:

   | Secret | What goes in it |
   |---|---|
   | `TAURI_SIGNING_PRIVATE_KEY` | the whole contents of `~/.tauri/geode.key` |
   | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the password you just chose |
   | `TAURI_SIGNING_PUBLIC_KEY` | the whole contents of `~/.tauri/geode.key.pub` |

   The public half is not a secret — it ships inside every binary — but it
   lives beside the other two so there is one place to look and one place to
   rotate.

3. Tag a release as usual. That is all; nothing else changes.

**What the `bundle` job then does.** Before building, the *Switch on signed
updater artifacts* step checks for both halves. With neither, it logs that
this release carries no manifest and changes nothing. With exactly one, it
fails the job on purpose: a signed bundle nobody can verify, and a key with
nothing signed for it, are both quietly broken, and quietly broken is the one
outcome worth stopping for. With both, it patches the checked-out
`src-tauri/tauri.conf.json` — `bundle.createUpdaterArtifacts` to `true` and
`plugins.updater.pubkey` to the public half — and the build step gets the
public half as `GEODE_UPDATER_PUBKEY` for the Rust compile and the private
half as `TAURI_SIGNING_PRIVATE_KEY` for the bundler.

The config is patched on the runner rather than committed because the bundler
reads the public key from `plugins > updater > pubkey` to cross-check what it
just signed, and a key committed there is a key you cannot rotate without a
commit. The runner's checkout is thrown away, so this costs nothing. The
binary gets its key from `option_env!` instead, the same route as the
CurseForge key and the Microsoft client id, and for the same reason.

`tauri-action` then writes `latest.json` beside the installers: one entry per
platform, each naming a bundle URL and its signature. That file is what an
installed Geode reads, from the endpoint in `tauri.conf.json`:

```
https://github.com/cody-cline/geode/releases/latest/download/latest.json
```

`latest/download` resolves to the most recent **published**, non-prerelease
release — so a draft nobody has published is invisible to every installed
Geode, which is the behaviour you want from a draft.

**A local build still cannot install an update** unless you give it a key. Put
the public half in `.env` as `GEODE_UPDATER_PUBKEY` and a build from your desk
will read the signed manifest like any other; leave it out and it reads the
releases API and only says what is out. Either way a local build can only ever
install what CI signed, which is the point. Worth knowing before you set it:
in `tauri dev` the thing an install would replace is the debug binary under
`target/`, not an installed Geode, so set it on a real bundle rather than on
the dev run.

**What refuses, and refuses quietly.** Each of these does nothing rather than
offering something unverified, because this is the one feature here that can
do real harm:

- no public key compiled in — no plugin is registered, so there is nothing to
  download and no way to install anything. The version is still compared and
  still reported, from a source that needs no key; what is refused is the
  install, which is the half that can do harm;
- a manifest endpoint that is not `https` — the plugin refuses it in a release
  build, and a unit test refuses it in the repository
  (`the_committed_config_names_an_https_endpoint_and_no_key`);
- a version that is not newer, so a stale or rolled-back manifest cannot walk
  somebody backwards;
- a version string that will not parse, which is never read as "newer".

None of the last three is reported as a reason. They are deliberately
indistinguishable from "nothing newer", because the only honest thing to do
with a manifest that does not make sense is to carry on with the build already
installed. The first is the exception, and that is the point of it: having no
key is said out loud, because unlike a stale manifest it is something the
person can do something about, and a version they have to fetch by hand is no
use to them unsaid.

**What refuses loudly, because it cannot do otherwise.** A signature is not
one of the four above, however much it belongs with them. There is no
signature to check at check time: `check_for_update` decides from the manifest
alone, and the signature covers the artefact, which does not exist on this
computer yet. So a tampered bundle behind a valid manifest is **offered**,
Settings says it is ready to install, pressing Install downloads all of it,
and only then does the check fail — with an error toast, and the installed
Geode untouched. Nothing unsafe is installed, which is the part that matters;
what it costs is a full transfer and a visible failure, and planning around
"no offer, no download, no message" would be planning around something else.

**What binds the version to the artefact.** `latest.json` is fetched over TLS
but nobody signs it, so its `version` field on its own proves nothing about
which release the `url` and `signature` beside it point at. Without more, a
compromised release host — or a stolen `GITHUB_TOKEN`, which needs no key at
all — could announce 9.9.9 over the url and signature of a genuine old
release, and every installed Geode would verify it happily and downgrade
itself. `requireSignedVersion` in `plugins.updater` is what closes that: the
Tauri CLI records the app version in each signature's trusted comment, which
minisign's own signature covers, and the plugin compares the two and refuses a
mismatch. It needs a signer that records one — **CLI 2.12.0 or newer**, which
is why `package-lock.json` pins it there and
`the_signed_version_check_is_on_and_the_pinned_signer_records_one` fails if
either half moves without the other. Turning the flag on against an older
signer makes every update fail with `MissingSignedVersion` instead, so re-sign
and re-publish anything people can still update from before changing it.

**What is bounded rather than refused.** The manifest fetch gets 30 seconds
and the download half an hour (`MANIFEST_TIMEOUT` and `DOWNLOAD_TIMEOUT` in
`src-tauri/src/update.rs`), because `reqwest` applies no request timeout of
its own and the plugin hands back an `Update` with none either — so a host
that accepts the connection and then says nothing would otherwise leave
Settings on its spinner for the life of the process. The download clock is
also the only ceiling on how large an artefact a manifest can name: the plugin
buffers the whole download into memory and its progress callback cannot refuse
a chunk, so there is no byte count to refuse at.

**It asks, and it waits.** Geode checks once at start and whenever somebody
opens Settings → About or presses Check again — never on a timer, and it
installs nothing on its own. The offer carries Install, Not now and Skip this
one — or, in a build with no key, the releases page in Install's place — and a
skip is for that single version: the next release is offered as usual.
Installing ends in a restart, and Geode runs Minecraft and servers as
child processes of itself, so an install is **refused while any game or server
is running** — a restart either kills a game outright or leaves it playing
with no launcher attached to its log, its console or its Stop button. A launch
that has begun but not yet spawned a JVM counts, and so does an install in
flight: both spend minutes rewriting an instance's files with no process to
show for it, and a restart through either leaves that instance half written.
See `restart_would_interrupt` in `src-tauri/src/update.rs`.

That question is asked **twice**, which matters more than it sounds. The first
ask is before the download, which is where the button's own reason comes from;
the second is after the download and before anything is written to disk.
Between them sit minutes of network transfer during which the window stays
fully live — `launch_instance` holds no lock against `install_update`, and a
pending `invoke` stops nobody opening Instances and pressing Play. What is
left is the gap between that second ask and the restart, which is local file
work rather than a transfer.

**What is wired and what is proven are not the same thing.** Nothing here has
updated a Geode. There is no signed release to update from, no keypair on this
machine, and no Windows or macOS to try one on. What the offline tests cover
is most of the decisions — the version comparison, the downgrade and
unparseable refusals, a skip and its expiry and the patch that clears one, the
refusal under a running game and under a launch or install still in flight,
the committed endpoint, the absence of a committed key, the signed-version
flag against the signer the lockfile pins, the tag the releases API hands back
and the shape it arrives in, the two endpoints naming one repository, that an
install cannot be reached or a button drawn without a key, and the states of
the copy Settings shows. What they cannot reach is the part that only a real
release exercises:

- the signature check itself, against a real key and a real bundle, and with
  it `requireSignedVersion`: that the CLI really does write `version:` into
  the trusted comment of a bundle it signs, and that the plugin reads it back
  and accepts a matching release. The pin and the flag are tested; the
  signature they describe has never been made;
- the two timeouts actually firing. Both are assignments into plugin fields,
  and proving the behaviour needs a hung endpoint and a live window;
- the second running-game check, for the same reason: it sits between a
  download and an install that no offline test can reach;
- `latest.json` having the shape the plugin expects, and
  `releases/latest/download` resolving to it;
- the releases API answering at all: no test here has made that request, so
  what a keyless build really gets back — the tag, the notes, the rate limit
  an unauthenticated IP is allowed — is pinned against the documented shape
  and nothing more;
- an AppImage replacing itself on disk and the restart coming back up on the
  new build;
- Windows, where the NSIS installer closes Geode and relaunches it, and macOS,
  where the `.app` is swapped — neither of which anybody has run;
- a Geode installed from the `.deb` or `.rpm`. The manifest's Linux entry is
  the AppImage, so an update there is expected to fail at the install step
  rather than do anything dangerous, and Settings says to take the new package
  by hand. Expected, not observed.

The first real release is where all of that gets tested. Cut it as a draft,
publish it, and update a previous build to it before telling anybody the
updater works.

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
2. Commit and push to `main`. Pushing builds nothing and checks Linux only.
3. **Actions → build → Run workflow**, tick **release**, and put the version
   in **tag** as `vX.Y.Z`. The action makes the tag itself, from whatever is
   on `main` when the run starts, so there is nothing to tag by hand.
4. `check` runs on three platforms, then `bundle` builds on three and opens a
   draft release.
5. Download the Linux package and run it. Say, in the release notes, which
   bundles were launched and which were not.
6. Publish the draft. Until it is published, `releases/latest/download` does
   not resolve to it, so no installed Geode can see the update — which also
   means publishing is the moment the manifest goes live to everybody.
7. If the updater is switched on, update a copy of the *previous* release to
   this one before saying anywhere that the updater works. It has never been
   done.

Step 1 matters more now than it used to: the version in `tauri.conf.json` is
what goes into `latest.json`, and an installed Geode compares that against its
own `CARGO_PKG_VERSION`. Those two disagreeing is how a release ends up either
invisible to every existing install or offered to the build it already is.

Nothing in this repository parses the workflow file — GitHub is the first
thing that reads it, and a syntax error there surfaces as a run that never
starts. Changing it is worth a manual run before a release depends on it.
