import { marked } from "marked";

/**
 * Turn a provider's description into safe HTML.
 *
 * The two platforms disagree on format: Modrinth stores Markdown, CurseForge
 * stores HTML. Guessing by content is unreliable (Markdown may contain inline
 * HTML), but Markdown is a superset in practice — `marked` passes HTML through
 * untouched — so everything goes through it, then through the sanitiser.
 */
export function renderProviderBody(raw: string): string {
  let html: string;
  try {
    html = marked.parse(raw, { async: false, gfm: true, breaks: false });
  } catch {
    // A malformed description should degrade to plain text, not an empty pane.
    html = escapeHtml(raw);
  }
  return sanitize(html);
}

function escapeHtml(s: string): string {
  const div = document.createElement("div");
  div.textContent = s;
  return div.innerHTML;
}

/**
 * Elements that never survive a provider's markup, whatever they hold.
 *
 * The four SMIL names at the end are there because the attribute loop in
 * `sanitize` keys on an attribute's *name*, and SMIL sets a URL from an
 * attribute nobody thinks to look at:
 * `<animate attributeName="href" values="javascript:…">` inside an `<svg><a>`
 * leaves the anchor's own `href` empty and its animated value set to the
 * payload. A description has no use for SMIL, so the elements go rather than
 * every attribute that might name another one being chased.
 */
export const DROPPED_ELEMENTS =
  "script, style, iframe, object, embed, form, link, meta, base, " +
  "animate, animateMotion, animateTransform, set";

/**
 * Whether an attribute goes on its name alone, whatever its value says.
 *
 * `on*` is every event handler. `style` is here because this window's CSP
 * allows inline style and nothing in `.prose` creates a containing block, so
 * `style="position:fixed;inset:0;z-index:99999"` out of a mod description drew
 * author-chosen content over the whole app frame with no boundary showing it
 * was not Geode. Nothing in a description needs it — `.prose` styles every tag
 * these descriptions use.
 */
export function attributeDropped(name: string): boolean {
  const lower = name.toLowerCase();
  return lower.startsWith("on") || lower === "style";
}

/**
 * Whether a URL out of provider markup may stay: an allowlist over the
 * *resolved* URL, not a denylist over the text.
 *
 * The text and the URL are not the same thing, and the gap is wide enough to
 * walk through. The URL parser strips every TAB, LF and CR from a scheme and
 * every leading C0 control, while nothing trims the middle of a string — so an
 * attribute written `java&#9;script:alert(1)` arrives here as
 * `java\tscript:alert(1)`, which no `startsWith("javascript:")` test catches
 * and which the parser then reads back as `javascript:`. Asking the same
 * parser what the scheme turned out to be closes that, and closes
 * `data:application/xhtml+xml`, `vbscript:` and whatever is next along with
 * it, instead of a list of names that grows one incident at a time.
 *
 * `base` is the document's own base URL, so a relative or `#fragment` URL is
 * judged by what it resolved to: this window's own origin, which is
 * `tauri://localhost` in a packaged build on Linux and so refused, and
 * `http://tauri.localhost` on Windows and so kept. Nothing dangerous can come
 * out of that either way, and a relative URL in a provider's description was
 * written against the provider's site and points at nothing here — so when the
 * attribute goes, the text stays, which is the right end for it.
 */
export function urlAllowed(value: string, base: string): boolean {
  let url: URL;
  try {
    url = new URL(value, base);
  } catch {
    // Not a URL at all, so there is nothing to keep and nothing lost.
    return false;
  }
  return url.protocol === "http:" || url.protocol === "https:" || url.protocol === "mailto:";
}

/**
 * Strip provider-supplied markup down to what this app is willing to render.
 *
 * This is HTML written by third-party authors, so it is treated as untrusted:
 * parse it detached from the document, drop every `DROPPED_ELEMENTS` element,
 * every attribute `attributeDropped` names, and every URL `urlAllowed`
 * refuses.
 *
 * That is a description of what it does, not a claim that it is enough. It is
 * a hand-written sanitiser over a parser with corners nobody here has mapped,
 * and the thing that actually stops a script running is the window's CSP —
 * `default-src 'self'` with no `script-src`, in `src-tauri/tauri.conf.json`.
 * For a while that backstop was carrying this on its own and the comment here
 * said otherwise; `richtext.test.mjs` now fails if the CSP is relaxed, because
 * relaxing it and trusting this function is the pair of edits that turns a mod
 * description into script in this webview.
 */
export function sanitize(html: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");

  doc.querySelectorAll(DROPPED_ELEMENTS).forEach((el) => el.remove());

  doc.querySelectorAll("*").forEach((el) => {
    for (const attr of [...el.attributes]) {
      const name = attr.name.toLowerCase();
      if (attributeDropped(attr.name)) {
        el.removeAttribute(attr.name);
      } else if (
        (name === "href" || name === "src" || name === "xlink:href") &&
        !urlAllowed(attr.value, document.baseURI)
      ) {
        el.removeAttribute(attr.name);
      }
    }
    if (el.tagName === "A") {
      el.setAttribute("target", "_blank");
      el.setAttribute("rel", "noreferrer noopener");
    }
  });

  return doc.body.innerHTML;
}
