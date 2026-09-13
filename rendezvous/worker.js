/**
 * The share-code lookup service.
 *
 * This is the only thing Geode asks you to deploy, and it deliberately does
 * almost nothing: it maps a key to a small JSON pointer saying where a server's
 * pack can be fetched from, and it forgets that mapping on a timer. It never
 * sees a share code, never sees a pack, and never stores a file. Bandwidth for
 * the actual bytes is between the two machines involved, not yours.
 *
 * Used for **server** shares only. A personal pack is a `.geodepack` file you
 * hand over directly, so it needs no lookup and no code.
 *
 * WHY IT NEVER SEES A CODE
 * Geode derives `key = base32(SHA-256("geode-share-v1" || code))[0..16]` on the
 * sharing machine and sends only that. The code itself stays between the people
 * swapping it. So a dump of this storage yields keys and pointers and no way to
 * work back to a code — and a pointer alone is useless without one, because the
 * host end checks the code too.
 *
 * WHY A DURABLE OBJECT RATHER THAN KV
 * One share is one Durable Object, addressed by its key. On the same free plan
 * a SQLite-backed Durable Object allows 100,000 row writes and 5 million row
 * reads a day, against Workers KV's 1,000 writes and 100,000 reads — and writes
 * are the limit that binds, because every share created is exactly one. The
 * object also expires itself with an alarm set to the second, which is tidier
 * than a TTL, and gives revocation somewhere to live.
 *
 * The trade is that a Durable Object is instantiated in one location while KV
 * is read from everywhere. That costs one round trip, on a single lookup, made
 * once before transferring megabytes — not worth optimising for.
 *
 * DEPLOY
 *   cd rendezvous
 *   npx wrangler login     # once, in a browser
 *   npx wrangler deploy
 *
 * `wrangler.toml` beside this file carries the binding and the migration, so
 * there is nothing to scaffold and nothing to paste. Put the resulting URL into
 * Geode's settings; there is no account for Geode itself to hold.
 *
 * To work on it, `npx wrangler dev` runs the whole thing locally — Durable
 * Object, alarms and all — with no account at all.
 */

/** A key is 16 characters of Crockford Base32 and nothing else. */
const KEY = /^[0-9ABCDEFGHJKMNPQRSTVWXYZ]{16}$/;

/** Refuse anything larger outright: this is a pointer store, not a file host. */
const MAX_BODY = 4096;

/**
 * Clamp the lifetime. A share that dies inside a minute is of no use to
 * anybody, and a month is past the point where anyone remembers sharing it.
 */
const MIN_TTL = 60;
const MAX_TTL = 60 * 60 * 24 * 30;

/**
 * A week. Long enough that somebody away for the weekend can still join, and
 * it costs nothing — a lifetime consumes no writes, and a few hundred bytes
 * against 5 GB is millions of entries.
 */
const DEFAULT_TTL = 60 * 60 * 24 * 7;

// A desktop app is not a browser origin, but the landing page below is.
const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET,PUT,DELETE,OPTIONS",
  "access-control-allow-headers": "content-type,authorization",
};

const json = (body, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { ...CORS, "content-type": "application/json", "cache-control": "no-store" },
  });

/**
 * A preflight answer, which must carry no body at all.
 *
 * 204 is one of the statuses the Fetch spec forbids a body on, and constructing
 * such a Response throws rather than ignoring it — so this cannot go through
 * `json()`. The landing page is a real browser origin, so this path is reached.
 */
const preflight = () => new Response(null, { status: 204, headers: CORS });

const str = (value, max) => (typeof value === "string" ? value.slice(0, max) : "");

/** The bearer token on a request, or "" — never null, so comparisons are safe. */
const bearer = (request) =>
  (request.headers.get("authorization") || "").replace(/^Bearer\s+/i, "");

/** A share that exists and has not passed its expiry. */
const live = (share) => !!share && share.expires * 1000 > Date.now();

/**
 * One share.
 *
 * Per key rather than one object holding every share, so nothing serialises
 * behind a single global instance and each share is created near whoever
 * created it. There is no listing across shares and no need for one — an admin
 * keeps their own revocation token.
 */
export class ShareRegistry {
  constructor(ctx) {
    this.ctx = ctx;
  }

  async fetch(request) {
    // Drain the body first, whatever we intend to answer.
    //
    // Responding while bytes are still unread leaves the connection
    // mis-framed, and it is the *next* request on that connection that then
    // fails — so a rejected share breaks an unrelated one, which is about the
    // most confusing failure available. Read once, up front, then decide.
    const text = request.body ? await request.text() : "";

    if (request.method === "GET") {
      // Belt and braces. The alarm should have cleared an expired share
      // already, but a read arriving in that gap must not resolve.
      const share = this.ctx.storage.kv.get("share");
      if (!live(share)) return json({ error: "no such share" }, 404);
      const { revoke, ...visible } = share;
      return json(visible);
    }

    if (request.method === "PUT") {
      // Replacing a live share belongs to whoever created it.
      //
      // Without this, holding the code is enough to overwrite the pointer, and
      // on a server share the admin hands that code to every player — so any
      // one of them could repoint it at their own machine and serve their own
      // jars to everybody else under the admin's name. Creating a share stays
      // open (nothing is there to protect yet); replacing one does not.
      const existing = this.ctx.storage.kv.get("share");
      if (live(existing) && existing.revoke !== bearer(request)) {
        return json(
          { error: "this share already exists — send its revocation token to replace it" },
          403,
        );
      }

      if (text.length > MAX_BODY) {
        return json({ error: "too large — this stores pointers, not files" }, 413);
      }

      let body;
      try {
        body = JSON.parse(text);
      } catch {
        return json({ error: "body must be JSON" }, 400);
      }
      if (!body || typeof body.ticket !== "string" || !body.ticket) {
        return json({ error: "a share needs a ticket" }, 400);
      }

      const ttl = Math.min(
        MAX_TTL,
        Math.max(MIN_TTL, Number(body.ttlSeconds) || DEFAULT_TTL),
      );
      const expires = Math.floor(Date.now() / 1000) + ttl;

      // The creator gets a token back so they can revoke early. Held here
      // rather than derived from the code, so an admin can hand the code to
      // fifty people and still keep the off switch themselves.
      //
      // Kept across a replacement rather than rotated: a server that moves
      // address re-PUTs its ticket, and rotating here would invalidate the
      // token the admin has already stored for the share they still own.
      const revoke = live(existing) ? existing.revoke : crypto.randomUUID();

      this.ctx.storage.kv.put("share", {
        v: 1,
        ticket: body.ticket,
        name: str(body.name, 120),
        mcVersion: str(body.mcVersion, 32),
        loader: str(body.loader, 32),
        bytes: Number(body.bytes) || 0,
        files: Number(body.files) || 0,
        revoke,
        expires,
      });

      // Clears itself at the second it expires. No cron, nothing to sweep, and
      // nothing left behind if Geode never calls DELETE.
      await this.ctx.storage.setAlarm(expires * 1000);

      return json({ ok: true, revoke, expiresIn: ttl });
    }

    if (request.method === "DELETE") {
      const share = this.ctx.storage.kv.get("share");
      if (!live(share)) return json({ error: "no such share" }, 404);
      if (share.revoke !== bearer(request)) {
        return json({ error: "wrong revocation token" }, 403);
      }
      await this.ctx.storage.deleteAll();
      return json({ ok: true });
    }

    return json({ error: "method not allowed" }, 405);
  }

  /** Its own expiry. */
  async alarm() {
    await this.ctx.storage.deleteAll();
  }
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (request.method === "OPTIONS") return preflight();

    // A landing page so a code can be sent as a link. The code travels in the
    // fragment, which browsers never send to a server — so this page can show
    // a code without this Worker ever learning one.
    if (url.pathname === "/" || url.pathname === "/join") {
      return new Response(LANDING, {
        headers: { "content-type": "text/html;charset=utf-8" },
      });
    }

    const match = url.pathname.match(/^\/s\/([^/]+)$/);
    if (!match) return json({ error: "not found" }, 404);

    const key = match[1].toUpperCase();
    // Validated before an object is addressed, so a malformed key cannot cause
    // one to be created. An expired share and a key that never existed both
    // answer 404 from the object itself, so neither can be probed for.
    if (!KEY.test(key)) return json({ error: "malformed key" }, 400);

    const id = env.SHARES.idFromName(key);
    return env.SHARES.get(id).fetch(request);
  },
};

/**
 * The landing page.
 *
 * Its only job is to show the code so somebody can copy it into Geode. The code
 * stays in the fragment and never reaches the server — the same promise the
 * desktop app makes.
 */
const LANDING = `<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Open in Geode</title>
<style>
  :root { color-scheme: dark }
  body { margin:0; min-height:100vh; display:grid; place-items:center;
         background:#0b0f14; color:#e6edf3;
         font:15px/1.6 system-ui,-apple-system,Segoe UI,sans-serif }
  main { text-align:center; padding:32px; max-width:34rem }
  code { display:inline-block; margin:18px 0; padding:12px 18px; border-radius:10px;
         background:#161d27; box-shadow:0 0 0 1px #212b38;
         font:600 20px ui-monospace,SF Mono,Menlo,monospace; letter-spacing:.06em }
  p { color:#99a6b5 }
</style>
<main>
  <h1>Open this pack in Geode</h1>
  <p>Copy the code below, then paste it into Geode under <b>Import &rsaquo; From a share code</b>.</p>
  <code id="code">—</code>
  <p id="note">This page never sends the code anywhere. It is only shown to you.</p>
</main>
<script>
  // The fragment is not transmitted to the server, by design.
  const code = decodeURIComponent(location.hash.slice(1)).trim();
  document.getElementById("code").textContent = code || "No code in this link";
  if (!code) {
    document.getElementById("note").textContent =
      "A share link looks like .../join#GEODE-XXXXX-XXXXX";
  }
</script>
`;
