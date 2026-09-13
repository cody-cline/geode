/**
 * A page for looking at the theme.
 *
 * The app itself cannot be browsed without the Tauri backend, so the only way
 * to judge a palette change is a page that uses the real tokens and the real
 * global classes. Reached at `?palette` in the dev server; it is not linked
 * from anywhere and is not part of the app.
 */
const SWATCHES: [string, string][] = [
  ["--bg", "page"],
  ["--surface", "cards, panels"],
  ["--surface-2", "inputs, hover"],
  ["--surface-3", "pressed, active"],
  ["--border", "hairlines"],
  ["--border-strong", "hover hairlines"],
  ["--text", "primary"],
  ["--text-dim", "secondary"],
  ["--text-faint", "tertiary"],
  ["--accent", "action"],
  ["--danger", "destructive"],
  ["--warn", "caution"],
  ["--info", "neutral note"],
];

export default function Palette() {
  return (
    <div style={{ padding: 32, display: "flex", flexDirection: "column", gap: 28 }}>
      <header>
        <h1 style={{ fontSize: 20, marginBottom: 4 }}>Theme</h1>
        <p className="hint">
          Real tokens, real component classes. Toggle the root `data-theme`
          attribute to compare light and dark.
        </p>
        <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
          <button
            className="btn btn-sm"
            onClick={() => document.documentElement.removeAttribute("data-theme")}
          >
            Dark
          </button>
          <button
            className="btn btn-sm"
            onClick={() =>
              document.documentElement.setAttribute("data-theme", "light")
            }
          >
            Light
          </button>
        </div>
      </header>

      <section>
        <span className="label">Palette</span>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
            gap: 10,
            marginTop: 10,
          }}
        >
          {SWATCHES.map(([token, use]) => (
            <div
              key={token}
              style={{
                borderRadius: "var(--radius)",
                boxShadow: "0 0 0 1px var(--border)",
                overflow: "hidden",
                background: "var(--surface)",
              }}
            >
              <div style={{ height: 52, background: `var(${token})` }} />
              <div style={{ padding: "8px 10px" }}>
                <div style={{ fontSize: 11.5, fontFamily: "ui-monospace, monospace" }}>
                  {token}
                </div>
                <div style={{ fontSize: 10.5, color: "var(--text-faint)" }}>{use}</div>
              </div>
            </div>
          ))}
        </div>
      </section>

      <section>
        <span className="label">Surfaces and elevation</span>
        <div style={{ display: "flex", gap: 12, marginTop: 10, flexWrap: "wrap" }}>
          {["--shadow-sm", "--shadow", "--shadow-lg"].map((s) => (
            <div
              key={s}
              style={{
                width: 180,
                padding: 16,
                borderRadius: "var(--radius)",
                background: "var(--surface)",
                boxShadow: `0 0 0 1px var(--border), var(${s})`,
                fontSize: 12,
              }}
            >
              {s}
            </div>
          ))}
        </div>
      </section>

      <section>
        <span className="label">Controls</span>
        <div
          style={{
            display: "flex",
            gap: 8,
            marginTop: 10,
            flexWrap: "wrap",
            alignItems: "center",
          }}
        >
          <button className="btn btn-primary">Primary</button>
          <button className="btn">Default</button>
          <button className="btn btn-ghost">Ghost</button>
          <button className="btn btn-danger">Danger</button>
          <button className="btn btn-sm">Small</button>
          <button className="btn" disabled>
            Disabled
          </button>
        </div>
        <div style={{ display: "flex", gap: 8, marginTop: 10, maxWidth: 620 }}>
          <input className="input" placeholder="Search instances" />
          <select className="select">
            <option>Relevance</option>
            <option>Downloads</option>
          </select>
        </div>
        <div style={{ display: "flex", gap: 6, marginTop: 10, flexWrap: "wrap" }}>
          <span className="badge">neutral</span>
          <span className="badge badge-accent">accent</span>
          <span className="badge badge-warn">warn</span>
          <span className="badge badge-modrinth">modrinth</span>
          <span className="badge badge-curseforge">curseforge</span>
          <span className="badge badge-spiget">spigot</span>
        </div>
      </section>

      <section>
        <span className="label">A card, as the library draws it</span>
        <div style={{ display: "flex", gap: 14, marginTop: 10, flexWrap: "wrap" }}>
          {["#3a5a78", "#6b4a2f", "#2f5c46"].map((hue, i) => (
            <article
              key={hue}
              style={{
                width: 250,
                borderRadius: "var(--radius)",
                background: "var(--surface)",
                boxShadow: "0 0 0 1px var(--border)",
                overflow: "hidden",
              }}
            >
              <div
                style={{
                  height: 130,
                  background: `linear-gradient(150deg, ${hue}, color-mix(in srgb, ${hue} 40%, #000))`,
                }}
              />
              <div style={{ padding: 12 }}>
                <div style={{ fontSize: 14, fontWeight: 600 }}>
                  {["All the Mods 10", "Create: Arcane", "Vanilla 1.21"][i]}
                </div>
                <div style={{ fontSize: 11.5, color: "var(--text-faint)", marginTop: 2 }}>
                  {["NeoForge · 384 mods", "Forge · 211 mods", "No loader"][i]}
                </div>
              </div>
            </article>
          ))}
        </div>
      </section>

      <section>
        <span className="label">Console</span>
        <pre
          style={{
            marginTop: 10,
            padding: 14,
            borderRadius: "var(--radius)",
            background: "var(--bg)",
            boxShadow: "0 0 0 1px var(--border)",
            fontFamily: "ui-monospace, monospace",
            fontSize: 11.5,
            lineHeight: 1.6,
            color: "var(--text-dim)",
          }}
        >
{`[00:12:04] [Server thread/INFO]: Starting minecraft server version 1.21.1
[00:12:05] [Server thread/WARN]: Failed to load eula.txt
[00:12:06] [Server thread/ERROR]: Encountered an unexpected exception
[00:12:09] [Server thread/INFO]: Done (4.812s)! For help, type "help"`}
        </pre>
      </section>
    </div>
  );
}
