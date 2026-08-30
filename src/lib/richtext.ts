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
 * Strip anything executable or framed out of provider-supplied markup.
 *
 * This renders HTML written by third-party authors, so it is treated as
 * untrusted: parse it detached from the document, drop script/style/iframe and
 * friends, and remove every `on*` handler and `javascript:` URL.
 */
export function sanitize(html: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");

  doc
    .querySelectorAll("script, style, iframe, object, embed, form, link, meta, base")
    .forEach((el) => el.remove());

  doc.querySelectorAll("*").forEach((el) => {
    for (const attr of [...el.attributes]) {
      const name = attr.name.toLowerCase();
      const value = attr.value.trim().toLowerCase();
      if (name.startsWith("on")) {
        el.removeAttribute(attr.name);
      } else if (
        (name === "href" || name === "src" || name === "xlink:href") &&
        (value.startsWith("javascript:") || value.startsWith("data:text/html"))
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
