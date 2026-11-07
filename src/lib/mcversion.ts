/**
 * Whether one Minecraft version is newer than another.
 *
 * Minecraft's numbering changed: `26.3` is newer than `1.21.11`, so the first
 * segment decides and nothing can be read off the string's shape. Two things
 * that look like they would work do not — text sorts `1.21.9` above
 * `1.21.10`, and a float cannot hold `1.21.1` at all — which is why this is a
 * segment-wise numeric comparison and not either of those.
 *
 * In `src/lib` rather than in the screen that asks, and in a `.ts` with no JSX
 * in it, for one reason: `npm test` can import this and actually run the
 * arithmetic. Everything else on this frontend is pinned by reading the text
 * of a `.tsx`, which would have pinned the wording of the warning and not the
 * sum behind it.
 */

/** A plain release: `1.21`, `1.21.4`, `26.3`. Never `26w14a` or `1.21-pre2`. */
const RELEASE = /^\d+(\.\d+)*$/;

/**
 * Whether `world` was saved by a Minecraft newer than the `instance` holding
 * it — the case where opening it is how a world gets lost.
 *
 * `false` when either side is not a plain release. A snapshot, or a string
 * some third-party jar wrote into `level.dat`, cannot be ordered against a
 * release, and a warning nobody can act on is worse than silence. Missing
 * segments read as zero, so `1.21` and `1.21.0` are one version.
 */
export function savedByNewer(world: string, instance: string): boolean {
  const [a, b] = [world.trim(), instance.trim()];
  if (!RELEASE.test(a) || !RELEASE.test(b)) return false;
  const [left, right] = [a, b].map((v) => v.split(".").map(Number));
  for (let i = 0; i < Math.max(left.length, right.length); i++) {
    const [mine, theirs] = [left[i] ?? 0, right[i] ?? 0];
    if (mine !== theirs) return mine > theirs;
  }
  return false;
}
