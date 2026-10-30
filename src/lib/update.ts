import type { StatusLine } from "./share";
import type { UpdateStatus } from "./types";

/** Where a new version comes from when Geode cannot fetch one itself. */
export const RELEASES_URL = "https://github.com/cody-cline/geode/releases";

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
 * What a build compiled without an updater key will not do, in the one clause
 * three of the lines below need.
 *
 * It is the whole difference between the two halves of this feature. Both look
 * and both compare; only a build with the key fetches and replaces anything,
 * because installing something nobody can prove the origin of is the one thing
 * an updater must never do.
 */
const CANNOT_INSTALL =
  "This copy was compiled without the key that proves an update came from the same place it did, so Geode will not download or replace anything itself";

/**
 * The line Settings shows about updating, and what the button beside it would
 * do.
 *
 * Three states, each of which reads differently depending on whether this
 * build can install what it found. An update that is waiting outranks a note
 * about an older one that was skipped.
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
  if (status.available) {
    return status.configured
      ? {
          tone: "info",
          title: `Geode ${status.available} is ready to install`,
          detail: `You have ${status.current}. Install downloads it, checks it was signed with Geode's own key, replaces this copy and restarts — nothing on this computer is written until that signature checks out.`,
        }
      : {
          tone: "info",
          title: `Geode ${status.available} is out, and this build cannot install it`,
          detail: `You have ${status.current}. ${CANNOT_INSTALL} — open the releases page, download ${status.available} and run it yourself.`,
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
    detail: status.configured
      ? "Nothing newer has been published. Geode looks once each time it starts, when this screen opens, and when you press Check again — never on a schedule of its own."
      : `Nothing newer has been published. Geode looks once each time it starts, when this screen opens, and when you press Check again — never on a schedule of its own. ${CANNOT_INSTALL}: when there is a new version you take it from the releases page and run the installer yourself.`,
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
 *
 * And it names the control that actually comes back, which is not the same one
 * in both halves: a keyless build's offer is a link to the releases page, so
 * promising Install there would promise the one button that panel never draws.
 */
export function putOffLine(version: string, configured: boolean): StatusLine {
  return {
    tone: "info",
    title: `Geode ${version} is waiting until you want it`,
    detail: `Nothing was installed and nothing was skipped. Geode offers ${version} again next time it starts, or press Show it again to bring ${configured ? "Install" : "the link to the releases page"} back now.`,
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

/**
 * The one line the Updates panel shows, chosen from four states that can hold
 * at the same time.
 *
 * The order is the whole of it, and the first branch is why this is a function
 * rather than a ternary in the panel: a check that failed leaves the status the
 * last one returned in place, so a re-check that cannot reach GitHub — rate
 * limited, offline, or a 404 before the first release exists — would otherwise
 * leave "Geode 0.1.0 is up to date" on screen. That is a green tick over a
 * question nobody answered, and a version check may say anything except that.
 *
 * Then "Not now", which takes Install off the screen and so has to take the
 * sentence describing Install with it. Then whatever the status says. Only a
 * status that has never arrived reads as still looking.
 */
export function panelLine(args: {
  status: UpdateStatus | null;
  failure: string | null;
  putOff: boolean;
}): StatusLine {
  const { status, failure, putOff } = args;
  if (failure) return checkFailedLine(failure);
  if (!status) {
    return {
      tone: "info",
      title: "Looking for a newer Geode",
      detail: "Asking GitHub which version is the latest.",
    };
  }
  if (status.available && putOff) return putOffLine(status.available, status.configured);
  return updateLine(status);
}

/**
 * The paragraph under the panel's buttons, which says how updating works here
 * — or says nothing, because that depends on which half of the feature this
 * build is.
 *
 * Null for a check that never came back. `configured` is only ever known from
 * a status, and a failed check leaves none: no network, a rate limit, or the
 * 404 the releases API answers until the first release exists. Choosing the
 * signed paragraph for that case told every build shipped so far — all of them
 * keyless — that Geode "replaces nothing until you press Install" and
 * "replaces the AppImage you are running", underneath a panel with no Install
 * button on it. Saying nothing about a mode nobody has established is the only
 * honest answer; the line above already says what went wrong and names both
 * ways out.
 */
export function panelHint(status: UpdateStatus | null): string | null {
  if (!status) return null;
  return status.configured
    ? "Geode never updates itself behind your back: it looks once each time it starts, when this screen opens, and when you ask — and replaces nothing until you press Install. Leaving an offer alone is “not now” — it comes back next time Geode starts. Installing replaces this copy and restarts, so it waits until no game and no server is running. On Linux it replaces the AppImage you are running; if you installed the .deb or .rpm, take the new package from the releases page instead."
    : "This build has no updater: it looks once each time it starts, when this screen opens, and when you ask, and that is all it does. Geode only installs a version it can prove came from the same place this copy did, and this one was compiled without the key to check that with — so nothing here downloads, replaces or restarts anything. A new version is a file you take from the releases page and install the way you installed this one. Leaving an offer alone is “not now” — it comes back next time Geode starts.";
}

/**
 * The toast at start, when a check nobody asked for found something.
 *
 * Here rather than written where it is raised because the two halves owe
 * different sentences — one points at a button that installs, the other at a
 * page to download from — and this is the file those words are tested in.
 * Called only once `available` is known to hold a version.
 */
export function availableToast(status: UpdateStatus): { title: string; body: string } {
  return {
    title: `Geode ${status.available} is available`,
    body: status.configured
      ? `You have ${status.current}. Settings → About says what installing it would do.`
      : `You have ${status.current}. Geode cannot install it itself — Settings → About opens the releases page to download it from.`,
  };
}
