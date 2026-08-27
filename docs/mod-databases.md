# Where Geode gets mods

The goal was one combined list across Modrinth and CurseForge — search once,
see everything, install from either. This is what was evaluated and why the
design landed where it did.

## The short version

**No third-party database indexes both platforms with install-grade metadata.**
"Install-grade" means, per file: a download URL, a checksum, the game versions
and loaders it targets, and its dependency graph. Aggregators have names and
links; they do not have that.

So Geode federates at query time instead: hit both APIs in parallel, normalise
into one model, merge duplicates into a single card. The user sees one list.
Under the hood there is no shared index and no third party in the path.

## What was evaluated

### Modrinth API v2 — the default source

Open, no key, no registration, generous rate limits. Rich metadata: every
version carries loaders, game versions, SHA-1 and SHA-512 hashes, and typed
dependencies (`required` / `optional` / `incompatible` / `embedded`). Also
exposes lookup-by-hash, which is how Geode can recognise a jar you dropped into
the folder by hand.

Used for search, installs, updates, and `.mrpack` import/export.

### CurseForge API v1 — opt-in, user's own key

Larger catalogue, especially for older versions and big kitchen-sink packs.
Two constraints shape how it is wired in:

1. **Keys are per-developer and non-transferable.** The 3rd-party API terms
   state a key may not be disclosed to any third party. A key compiled into a
   public app is exactly that. So Geode ships without one and asks for yours in
   *Settings → Mod sources*. CurseForge search, installs and pack imports stay
   inert until it is set, and the UI says so rather than silently returning
   half a result set.

2. **Authors can disable third-party downloads.** When `allowModDistribution`
   is false the API returns a null `downloadUrl`. The CDN path is guessable
   from the file id, and some launchers reconstruct it. Geode does not — that
   setting is the author's decision. Those projects get a **Manual** badge and
   a link to their page.

### CurseRinth — rejected, unmaintained

A community proxy that re-serves the CurseForge API in Modrinth's shape. It
would have collapsed both platforms behind one client. The public instance
returns `{"error":"internal_error"}` today, the project has seen little
activity, and routing every user's queries through one volunteer's server with
one volunteer's API key inherits both a reliability problem and the same terms
problem. Not viable.

### Modpack Index — useful, but not as an install source

Free, keyless, and genuinely does span CurseForge, Modrinth and FTB. Its real
asset is cross-linking: each mod record carries both platform URLs.

```json
{ "name": "Just Enough Items (JEI)",
  "links": { "curseforge": "…/mc-mods/jei", "modrinth": "…/mod/jei" } }
```

It was not adopted, for three reasons:

- **No files.** No download URLs, hashes, or per-version loader/game-version
  data. It cannot install anything.
- **Modpack-derived coverage.** It indexes mods it finds *in modpacks*. A mod
  released last week that no pack has picked up yet is not there — and "even
  newly released mods" was a requirement.
- **Latency.** Adding a third API call to every keystroke to improve matching
  that slug comparison already handles is a bad trade for a launcher that is
  supposed to feel instant.

It remains the best option if cross-platform identity ever needs to be more
rigorous than the current heuristic. The API shape is recorded here so that
work does not start from scratch.

## How merging works

Implemented in `src-tauri/src/providers/mod.rs`.

Both providers are queried concurrently. Results normalise to a common
`Project`, then collapse on an identity key: the slug, normalised to lowercase
alphanumerics, falling back to the normalised display name when the slug is too
short to be distinctive.

That is deliberately simple, and it works because slugs agree far more often
than not — `sodium`, `jei`, `create`, `iris` are the same string on both
platforms. Punctuation differences (`just-enough-items` vs `justenoughitems`)
normalise away.

When a project is found on both:

- Modrinth becomes the canonical entry. Its API is open, its metadata is
  richer, and its downloads are never author-blocked.
- CurseForge is attached as `alsoOn`, so the detail drawer offers a source
  switch and the user can install from either.
- Download counts are summed. Per-platform counts answer "how popular is this
  mod *here*"; the sum answers the question the user is actually asking.

Sorting by downloads, followers or date re-ranks across the merged set.
Relevance does not — the two APIs have no shared relevance scale, so the merged
order is left interleaved rather than pretending one exists.

### Known limits

- A project published under genuinely different slugs *and* different display
  names on each platform will appear twice. Installing both would mean two jars
  of the same mod, which the loader will complain about. Modpack Index
  cross-links are the fix if this proves common.
- Pagination offsets are per-provider, and merging shrinks the result count, so
  "Load more" pages by page number rather than by rows on screen. Deduplication
  against already-shown results keeps repeats out.
- CurseForge caps `pageSize` at 50; Modrinth allows 100. Geode requests 30 per
  page from each.

## Adding another source

`Provider` is an enum with one module per platform behind it. A new source
needs to map its API onto `Project`, `Version`, `VersionFile` and `Dependency`,
then join the parallel fan-out in `providers::search`. Nothing above that layer
knows how many providers exist.
