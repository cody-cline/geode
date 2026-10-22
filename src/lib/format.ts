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

/** Short form, for rows too tight for the full name. */
export function providerShort(provider: string): string {
  if (provider === "modrinth") return "MR";
  if (provider === "curseforge") return "CF";
  if (provider === "spiget") return "Spigot";
  return provider.slice(0, 2).toUpperCase();
}

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
