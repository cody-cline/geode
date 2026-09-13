import type { CommandNode, ServerLine } from "./types";

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

/** One completion offered for the word being typed. */
export interface Suggestion {
  name: string;
  /** Argument names that follow it, shown as a hint rather than completed. */
  args: string[];
}

/** Split console input into whole words plus the word still being typed. */
function tokenise(input: string): { done: string[]; partial: string } {
  const text = input.replace(/^\//, "");
  const parts = text.split(/\s+/);
  // A trailing space means the last word is finished and the next one is open.
  const partial = /\s$/.test(text) ? "" : (parts.pop() ?? "");
  return { done: parts.filter((p) => p.length > 0), partial };
}

/**
 * What could come next, given what has been typed.
 *
 * The tree is the server's own, so this completes a mod's subcommands exactly
 * as it completes vanilla's — and stops at arguments, because `<player>`
 * describes what to type rather than being something to type. Those are
 * returned alongside as a hint.
 */
export function suggest(input: string, tree: CommandNode[]): Suggestion[] {
  const { done, partial } = tokenise(input);

  let level = tree;
  for (const word of done) {
    const node = level.find((n) => n.name === word);
    // An unknown word means we are past anything the tree can speak to —
    // inside an argument, most likely.
    if (!node) return [];
    level = node.children;
  }

  const typed = partial.toLowerCase();
  const starts = level.filter((n) => n.name.toLowerCase().startsWith(typed) && n.name !== partial);
  const contains = level.filter(
    (n) => !n.name.toLowerCase().startsWith(typed) && n.name.toLowerCase().includes(typed),
  );
  const by = (a: CommandNode, b: CommandNode) => a.name.localeCompare(b.name);
  return [...starts.sort(by), ...contains.sort(by)]
    .slice(0, 8)
    .map((n) => ({ name: n.name, args: n.args }));
}

/** Replace the word being typed with `name`, leaving the rest alone. */
export function complete(input: string, name: string): string {
  const slash = input.startsWith("/") ? "/" : "";
  const { done } = tokenise(input);
  return slash + [...done, name].join(" ") + " ";
}
