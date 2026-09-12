import type { ServerLine } from "./types";

/**
 * Reading a server's console output.
 *
 * A server writes plain text, not the structured log4j XML the game client
 * emits, so the shape is read back out of the line itself:
 *
 * ```
 * [03:52:13] [Server thread/INFO]: Done (5.123s)! For help, type "help"
 * ```
 *
 * Forge and NeoForge put a longer thread name in the same slot, and some lines
 * have no prefix at all — a stack trace continuation, or anything a mod writes
 * straight to stdout. Those come back as message-only, which is the honest
 * answer rather than forcing them into a shape they do not have.
 */
const PREFIXED = /^\[([^\]]+)\]\s*\[([^\]]*?)\/([A-Z]+)\]:?\s?(.*)$/;

export interface ParsedLine {
  time?: string;
  thread?: string;
  level: string;
  message: string;
}

export function parseLine(entry: ServerLine): ParsedLine {
  const match = PREFIXED.exec(entry.line);
  if (!match) {
    // stderr is a level in its own right: a JVM crash arrives there with no
    // prefix at all, and colouring it like ordinary output would bury it.
    return { level: entry.stream === "err" ? "error" : "info", message: entry.line };
  }
  const [, time, thread, rawLevel, message] = match;
  return {
    time,
    thread,
    level: entry.stream === "err" ? "error" : normaliseLevel(rawLevel),
    message,
  };
}

function normaliseLevel(raw: string): string {
  const level = raw.toLowerCase();
  if (level === "warn" || level === "warning") return "warn";
  if (level === "error" || level === "fatal" || level === "severe") return "error";
  if (level === "debug" || level === "trace") return "debug";
  return "info";
}

/** One highlighted run inside a message. */
export interface Piece {
  text: string;
  kind?: "player" | "number" | "quoted" | "address" | "path" | "fault";
}

/**
 * Patterns worth colouring inside a message.
 *
 * Kept deliberately narrow. A pattern that fires on ordinary prose makes the
 * whole console harder to read than no highlighting at all, so there is no
 * attempt at player names — they are unquoted bare words and indistinguishable
 * from any other word in a sentence.
 */
const PIECE = new RegExp(
  [
    // A quoted string, single or double.
    `"[^"]*"|'[^']*'`,
    // An address with a port, and bare IPv4.
    String.raw`\b\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?\b`,
    // A UUID.
    String.raw`\b[0-9a-fA-F]{8}-(?:[0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12}\b`,
    // Something that is plainly a fault, wherever it appears in the line.
    String.raw`\b(?:Exception|Caused by|WARN|ERROR|FAILED|Failed|failed)\b`,
    // A file or package path.
    String.raw`\b[\w.]+\.(?:jar|yml|json|toml|properties|log)\b`,
    // Durations and counts, including the "Done (5.123s)!" startup line.
    String.raw`\b\d+(?:\.\d+)?(?:s|ms|%)?\b`,
  ].join("|"),
  "g",
);

function kindOf(text: string): Piece["kind"] {
  if (text.startsWith('"') || text.startsWith("'")) return "quoted";
  if (/^\d{1,3}(\.\d{1,3}){3}/.test(text)) return "address";
  if (/^[0-9a-fA-F]{8}-/.test(text)) return "player";
  if (/^(Exception|Caused by|WARN|ERROR|FAILED|Failed|failed)$/.test(text)) return "fault";
  if (/\.(jar|yml|json|toml|properties|log)$/.test(text)) return "path";
  return "number";
}

/** Split a message into plain and highlighted runs, in order. */
export function pieces(message: string): Piece[] {
  const out: Piece[] = [];
  let at = 0;
  for (const match of message.matchAll(PIECE)) {
    const start = match.index ?? 0;
    if (start > at) out.push({ text: message.slice(at, start) });
    out.push({ text: match[0], kind: kindOf(match[0]) });
    at = start + match[0].length;
  }
  if (at < message.length) out.push({ text: message.slice(at) });
  return out;
}

/**
 * Commands a vanilla dedicated server accepts, as a starting point.
 *
 * Not the whole truth and not meant to be: mods and plugins register into the
 * same dispatcher and are indistinguishable from these, so anything the server
 * itself reports is folded in on top by `harvest`. The leading slash is
 * optional at a server console, so these are stored without one.
 */
export const VANILLA_COMMANDS = [
  "advancement", "attribute", "ban", "ban-ip", "banlist", "bossbar", "clear",
  "clone", "damage", "data", "datapack", "debug", "defaultgamemode", "deop",
  "difficulty", "effect", "enchant", "execute", "experience", "fill",
  "fillbiome", "forceload", "function", "gamemode", "gamerule", "give", "help",
  "item", "jfr", "kick", "kill", "list", "locate", "loot", "me", "msg", "op",
  "pardon", "pardon-ip", "particle", "perf", "place", "playsound", "random",
  "recipe", "reload", "return", "ride", "rotate", "save-all", "save-off",
  "save-on", "say", "schedule", "scoreboard", "seed", "setblock", "setidletimeout",
  "setworldspawn", "spawnpoint", "spectate", "spreadplayers", "stop",
  "stopsound", "summon", "tag", "team", "teammsg", "teleport", "tell",
  "tellraw", "tick", "time", "title", "tp", "transfer", "trigger", "version",
  "waypoint", "weather", "whitelist", "worldborder", "xp",
];

/**
 * Commands the server itself has mentioned.
 *
 * Two shapes, because the two families answer `help` completely differently.
 * Vanilla — and so Forge, NeoForge and Fabric, which all keep vanilla's help —
 * prints one usage line per command: `/give <targets> <item>`. Paper and Spigot
 * replace it with Bukkit's index: `/give: Gives an item to a player`.
 *
 * This is how a *mod's* commands become known, since a mod registers them in
 * code and nothing in its jar says so. A plugin is the easier case and does not
 * depend on this: Bukkit makes it declare its commands in `plugin.yml`, which
 * Geode reads off the jar with the server stopped.
 *
 * Nothing is ever sent to get this. Running `help` dumps ninety-odd lines into
 * the console the user is reading, so it happens when they ask for it — by
 * running it themselves, or with the console's own button.
 */
export function harvest(lines: ServerLine[]): string[] {
  const found = new Set<string>();
  for (const entry of lines) {
    const { message } = parseLine(entry);
    const match = /^\/([a-z0-9_-]+)(?::| |$)/i.exec(message.trim());
    if (match) found.add(match[1].toLowerCase());
  }
  return [...found];
}

/** Commands matching what has been typed so far, best first. */
export function suggest(input: string, known: string[]): string[] {
  const typed = input.replace(/^\//, "").toLowerCase();
  // Only the first word is a command; after a space the argument is theirs.
  if (!typed || typed.includes(" ")) return [];
  const starts = known.filter((c) => c.startsWith(typed) && c !== typed);
  const contains = known.filter((c) => !c.startsWith(typed) && c.includes(typed));
  return [...starts.sort(), ...contains.sort()].slice(0, 8);
}
