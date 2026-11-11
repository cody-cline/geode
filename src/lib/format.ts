import type { GameVersion } from "./types";

export function compactNumber(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}K`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0)}M`;
  return `${(n / 1_000_000_000).toFixed(1)}B`;
}

export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  // A whole server is gigabytes, and "12288.0 MB" is a number nobody reads.
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/** "3 days ago" style, for last-played and last-updated stamps. */
export function relativeTime(input: string | number | null | undefined): string {
  if (input == null) return "never";
  const then = typeof input === "number" ? input * 1000 : Date.parse(input);
  if (Number.isNaN(then)) return "unknown";

  const seconds = Math.round((Date.now() - then) / 1000);
  if (seconds < 60) return "just now";

  const units: [number, Intl.RelativeTimeFormatUnit][] = [
    [60, "minute"],
    [24, "hour"],
    [7, "day"],
    [4.348, "week"],
    [12, "month"],
    [Number.POSITIVE_INFINITY, "year"],
  ];

  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  let value = seconds / 60;
  for (const [step, unit] of units) {
    if (Math.abs(value) < step) {
      return formatter.format(-Math.round(value), unit);
    }
    value /= step;
  }
  return "a long time ago";
}

/**
 * When each Minecraft version came out, as `YYYY-MM-DD`, keyed by its id.
 *
 * Read from the version manifest rather than worked out from the string,
 * because the string stopped saying: 26.3 is newer than 1.21.11 and nothing
 * about the two numbers tells anybody that. So a screen offering a choice
 * between versions prints the date beside each one — the New instance list
 * does, and the two screens that rank targets do it from this, in this shape,
 * so none of the three can place a version in time differently.
 *
 * A version the manifest has nothing for is absent rather than guessed: a
 * provider can name a game version Mojang never shipped, and an invented date
 * is worse than none.
 */
export function releaseDates(versions: GameVersion[]): Map<string, string> {
  return new Map(versions.map((v) => [v.id, v.releaseTime.slice(0, 10)]));
}

/**
 * Display name for a content source.
 *
 * A function rather than a ternary at each call site: there are three sources
 * now, and every `x === "modrinth" ? … : …` quietly labelled the third one
 * CurseForge.
 */
export function providerLabel(provider: string): string {
  if (provider === "modrinth") return "Modrinth";
  if (provider === "curseforge") return "CurseForge";
  if (provider === "spiget") return "SpigotMC";
  return titleCase(provider);
}

/**
 * What Minecraft calls each `GameType`, for the badge on a world's row.
 *
 * A label map and not a mirror of anything: `World.gameMode` crosses the IPC
 * boundary as the number the game writes, because naming them is this side's
 * job — see the field's own comment in `instance/worlds.rs`.
 */
export const GAME_MODES: Record<number, string> = {
  0: "Survival",
  1: "Creative",
  2: "Adventure",
  3: "Spectator",
};

export function titleCase(s: string): string {
  return s
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((w) => w[0].toUpperCase() + w.slice(1))
    .join(" ");
}

/** A stable, pleasant colour per instance, used when there is no icon. */
export function hashHue(input: string): number {
  let hash = 0;
  for (let i = 0; i < input.length; i++) {
    hash = (hash << 5) - hash + input.charCodeAt(i);
    hash |= 0;
  }
  return Math.abs(hash) % 360;
}

export function initials(name: string): string {
  const words = name.trim().split(/[\s_-]+/).filter(Boolean);
  if (words.length === 0) return "?";
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[1][0]).toUpperCase();
}

/**
 * The name on disk with the suffix that turns a file off taken back off it.
 *
 * `.disabled` is how a loader is made to ignore a jar, and it is nothing the
 * person reading the row did: they installed FerriteCore, not a renamed file.
 * The row says it is disabled in a word, so printing the suffix only repeats
 * that in the filesystem's terms. Display only — `fileName` is still the row's
 * identity, the key a selection is held by, and the name every command takes;
 * `strip_disabled` in `instance/content.rs` is the same rule on the side that
 * does the renaming.
 *
 * Here rather than in either Content tab because there are two of them, the
 * instance's and the server's, and they are one screen in two panes.
 */
export function shownName(fileName: string): string {
  return fileName.replace(/\.disabled$/, "");
}

/**
 * Whether a row's title is already the file's name, so the row should not go
 * on to print it a second time.
 *
 * A mod Geode installed has a title from its provider — "FerriteCore" over
 * `ferritecore-7.0.0.jar`, which is two facts. A jar somebody dropped into the
 * folder themselves has no provider and nothing else to be called, so the
 * title falls back to the file name and the line beneath it repeated that name
 * word for word, in a second font, as though it were telling you something.
 * Every untracked row said everything it had to say twice.
 *
 * Beside `shownName` because it is the same comparison: the suffix is off
 * both sides or the renamed file never matches the title it was given.
 */
export function titleIsFileName(fileName: string, name: string): boolean {
  return shownName(fileName) === name;
}
