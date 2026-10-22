import type { StatusLine } from "./share";
import type { UpdateStatus } from "./types";

/** Where a new version comes from when Geode cannot fetch one itself. */
export const RELEASES_URL = "https://github.com/littlesquirt1/geode/releases";

/**
 * Somebody else's words — a backend error, a network stack's — ended with one
 * full stop.
 *
 * `share.ts` has the same four lines, and they stay separate on purpose: this
 * file is loaded straight into Node by its test, which has no bundler to
 * resolve an extensionless import, so it may import types and nothing else.
 */
function sentence(text: string): string {
  const trimmed = text.trim();
  return /[.!?]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}

/**
 * The line Settings shows about updating, and what pressing Install would do.
 *
 * Four states, and the order matters: a build with no key can say nothing
 * about versions at all, and an update that is waiting outranks a note about
 * an older one that was skipped.
 *
 * Nothing here reports a *reason* Geode declined to offer a version it heard
 * about — a downgrade, an unreadable version, one that was skipped. Those are
 * deliberately indistinguishable from "nothing newer", because the only honest
 * thing to do with a manifest that does not make sense is to carry on with the
 * build already installed and not make it somebody's problem.
 *
 * A signature that does not check out is **not** one of those. There is no
 * signature to check until the artefact has been downloaded in full, so a
 * tampered bundle behind a valid manifest is offered, downloaded, and then
 * refused out loud with an error — nothing unsafe is installed, but it is not
 * a quiet no and this copy must not imply that it is.
 */
export function updateLine(status: UpdateStatus): StatusLine {
  if (!status.configured) {
    return {
      tone: "info",
      title: "This build cannot update itself",
      detail:
        "Geode only offers an update it can prove came from the same place this build did, and this copy was compiled without the key to check that with. New versions are on the releases page.",
    };
  }
  if (status.available) {
    return {
      tone: "info",
      title: `Geode ${status.available} is ready to install`,
      detail: `You have ${status.current}. Install downloads it, checks it was signed with Geode's own key, replaces this copy and restarts — nothing on this computer is written until that signature checks out.`,
    };
  }
  if (status.skipped) {
    return {
      tone: "info",
      title: `Geode ${status.skipped} is being skipped`,
      detail: `You have ${status.current}. Skipping is for that one version, so the next release is offered as usual — or stop skipping to be offered ${status.skipped} again now.`,
    };
  }
  return {
    tone: "ok",
    title: `Geode ${status.current} is up to date`,
    detail:
      "Nothing newer has been published. Geode looks once each time it starts, when this screen opens, and when you press Check again — never on a schedule of its own.",
  };
}

/**
 * The line while an offer is set aside by "Not now".
 *
 * Its own line because the ready-to-install one describes Install, and "Not
 * now" takes Install off the screen: a panel that explains a button nobody can
 * see is worse than one that says nothing. Nothing was installed and nothing
 * was skipped, so this says both, and names the way back — which is local,
 * rather than the network check that Check again would run to undo a decision
 * made here.
 */
export function putOffLine(version: string): StatusLine {
  return {
    tone: "info",
    title: `Geode ${version} is waiting until you want it`,
    detail: `Nothing was installed and nothing was skipped. Geode offers ${version} again next time it starts, or press Show it again to bring Install back now.`,
  };
}

/**
 * The line when the check itself failed and there is nothing to report but
 * that.
 *
 * Needed because a failed check leaves no status at all, and every control in
 * the panel used to be drawn from one: with no network, or before the first
 * release exists and the manifest 404s, the panel sat on "Looking for a newer
 * Geode" for ever with no retry, no link and no version comparison. Saying
 * what went wrong is the smaller half of the fix; having anything on screen to
 * press is the larger one.
 */
export function checkFailedLine(message: string): StatusLine {
  return {
    tone: "warn",
    title: "Could not check for a newer Geode",
    detail: `${sentence(message)} Nothing changed and nothing was installed. Press Check again, or take a new version from the releases page yourself.`,
  };
}
