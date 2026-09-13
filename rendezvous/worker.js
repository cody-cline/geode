/**
 * The share-code lookup service.
 *
 * This is the only thing Geode asks you to deploy, and it deliberately does
 * almost nothing: it maps a key to a small JSON pointer saying where a pack can
 * be fetched from, and it forgets that mapping on a timer. It never sees a
 * share code, never sees a pack, and never stores a file. Bandwidth for the
 * actual bytes is between the two machines involved, not yours.
 *
 * What is stored per share is a few hundred bytes. On Cloudflare's free tier
 * the limits that matter are KV writes (1,000/day) and reads (100,000/day),
 * which is one write per share created and one read per person joining.
 *
 * WHY IT NEVER SEES A CODE
 * Geode derives `key = base32(SHA-256("geode-share-v1" || code))[0..16]` on the
 * sharing machine and sends only that. The code itself stays between the people
 * swapping it. So a dump of this KV namespace yields keys and pointers and no
 * way to work back to a code — and a pointer alone is useless without one,
 * because the host end checks the code too.
 *
 * DEPLOY
 *   npm create cloudflare@latest -- geode-rendezvous
 *   # replace src/index.js with this file, then in wrangler.toml:
 *   #   [[kv_namespaces]]
 *   #   binding = "SHARES"
 *   #   id = "<from: wrangler kv namespace create SHARES>"
 *   npx wrangler deploy
 *
 * Then put the resulting URL into Geode's settings. Nothing else is required
 * and there is no account for Geode to hold.
 */

/** A key is 16 characters of Crockford Base32 and nothing else. */
const KEY = /^[0-9ABCDEFGHJKMNPQRSTVWXYZ]{16}$/;

/** Refuse anything larger outright: this is a pointer store, not a file host. */
const MAX_BODY = 4096;

/** Clamp the lifetime. Cloudflare's own floor for an expiring key is 60s. */
const MIN_TTL = 60;
const MAX_TTL = 60 * 60 * 24 * 30;
const DEFAULT_TTL = 60 * 60 * 24;

const json = (body, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type": "application/json",
      // A desktop app is not a browser origin, but the landing page below is.
      "access-control-allow-origin": "*",
      "access-control-allow-methods": "GET,PUT,DELETE,OPTIONS",
      "access-control-allow-headers": "content-type,authorization",
      "cache-control": "no-store",
    },
  });

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (request.method === "OPTIONS") return json({}, 204);

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
    if (!KEY.test(key)) return json({ error: "malformed key" }, 400);

    if (request.method === "GET") {
      const found = await env.SHARES.get(key);
      // An expired share and a share that never existed are the same answer.
      // Telling them apart would let somebody probe for keys that once worked.
      if (!found) return json({ error: "no such share" }, 404);
      return json(JSON.parse(found));
    }

    if (request.method === "PUT") {
      const text = await request.text();
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

      // The creator gets a token back so they can revoke early. It is stored
      // alongside rather than derived, so revoking needs the token and not the
      // code — an admin can hand a code out and still retain the off switch.
      const revoke = crypto.randomUUID();
      const ttl = Math.min(
        MAX_TTL,
        Math.max(MIN_TTL, Number(body.ttlSeconds) || DEFAULT_TTL),
      );

      await env.SHARES.put(
        key,
        JSON.stringify({
          v: 1,
          ticket: body.ticket,
          name: typeof body.name === "string" ? body.name.slice(0, 120) : "",
          mcVersion: typeof body.mcVersion === "string" ? body.mcVersion.slice(0, 32) : "",
          loader: typeof body.loader === "string" ? body.loader.slice(0, 32) : "",
          bytes: Number(body.bytes) || 0,
          files: Number(body.files) || 0,
          revoke,
          expires: Math.floor(Date.now() / 1000) + ttl,
        }),
        // Cloudflare deletes it. There is no cleanup job to run and nothing
        // accumulates if Geode never calls DELETE.
        { expirationTtl: ttl },
      );

      return json({ ok: true, revoke, expiresIn: ttl });
    }

    if (request.method === "DELETE") {
      const token = (request.headers.get("authorization") || "").replace(/^Bearer\s+/i, "");
      const found = await env.SHARES.get(key);
      if (!found) return json({ error: "no such share" }, 404);
      if (JSON.parse(found).revoke !== token) {
        return json({ error: "wrong revocation token" }, 403);
      }
      await env.SHARES.delete(key);
      return json({ ok: true });
    }

    return json({ error: "method not allowed" }, 405);
  },
};

/**
 * The landing page.
 *
 * Its only job is to show the code and offer to hand it to Geode. It does the
 * hashing in the browser so that the code stays in the fragment and never
 * reaches the server — which is the same promise the desktop app makes.
 */
const LANDING = `<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Open in Geode</title>
<style>
  :root { color-scheme: dark }
  body { margin:0; min-height:100vh; display:grid; place-items:center;
         background:#0d0c0b; color:#ece9e4;
         font:15px/1.6 system-ui,-apple-system,Segoe UI,sans-serif }
  main { text-align:center; padding:32px; max-width:34rem }
  code { display:inline-block; margin:18px 0; padding:12px 18px; border-radius:8px;
         background:#1c1b18; box-shadow:0 0 0 1px #2b2925;
         font:600 20px ui-monospace,SF Mono,Menlo,monospace; letter-spacing:.06em }
  p { color:#a39d94 }
  a { color:#d2a24c }
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
