// What the sharing screens say, kept apart from the screens so it can be
// tested on its own. Nothing here may import a value — only types — because
// the tests load this file straight into Node, which has no bundler to resolve
// the app's extensionless imports.

import type {
  ExistingInstance,
  JoinInput,
  JoinOutcome,
  JoinPreview,
  ShareStatus,
} from "./types";

/**
 * The port a typed address is looked up on when it has none. The backend's
 * default, not this machine's setting: a player's Geode has no way to know
 * what port an admin picked.
 */
export const DEFAULT_SHARE_PORT = 25590;

/**
 * A port typed into settings, or null when it is not one Geode can use.
 *
 * Below 1024 needs administrator rights to listen on in most systems, and a
 * launcher that silently fails to bind is worse than one that says no up front.
 */
export function parsePort(text: string): number | null {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const port = Number(trimmed);
  return port >= 1024 && port <= 65535 ? port : null;
}

export interface JoinForm {
  mode: "link" | "address";
  link: string;
  address: string;
  code: string;
}

/** What to ask the backend to preview, or null while the form is incomplete. */
export function joinInput(form: JoinForm): JoinInput | null {
  if (form.mode === "link") {
    const link = form.link.trim();
    return link ? { link } : null;
  }
  const address = form.address.trim();
  // A link pasted into the address box is still a link, and the better kind:
  // it carries the fingerprint, where a typed address is trusted on first use.
  if (/^geode:/i.test(address)) return { link: address };
  const code = form.code.trim();
  return address && code ? { address, code } : null;
}

/** One join dialog's worth of intent: the link it was opened with, if any. */
export interface JoinRequest {
  /** Unique for the session, so a replaced dialog remounts from scratch. */
  seq: number;
  link: string | null;
}

/**
 * The join dialog after a link arrives, or someone asks to join by hand.
 *
 * There is only ever one dialog. A different link replaces it; the same link
 * again — the parked copy and the event both delivering it, or StrictMode
 * running an effect twice — leaves it alone, so a preview in progress is not
 * thrown away and started over. The identity of the returned object is what
 * says whether anything changed.
 */
export function nextJoin(
  current: JoinRequest | null,
  link: string | null,
  seq: number,
): JoinRequest {
  if (current && (link === null || link === current.link)) return current;
  return { seq, link };
}

/**
 * What the join dialog offers next.
 *
 * Once an import has made an instance, nothing offers to import again — not
 * even after one that came back with problems — because a second import makes
 * a second instance rather than finishing the first. A server already joined
 * with this password offers the instance it made the first time, for the same
 * reason.
 */
export function joinStep(
  preview: JoinPreview | null,
  outcome: JoinOutcome | null,
): "lookUp" | "import" | "openExisting" | "done" {
  if (outcome) return "done";
  if (!preview) return "lookUp";
  return preview.existing ? "openExisting" : "import";
}

/**
 * What the join dialog says about the instance this server was already joined
 * into, and what opening it does.
 *
 * A link that carries a new password is saved onto that instance when it is
 * opened, which is the one place a link decides where an instance takes its
 * updates from ever after. So when it also carries a fingerprint the instance
 * does not know, the player is told: that is an admin who reinstalled Geode,
 * and it is equally what someone else answering at that address looks like.
 * With the same password there is nothing to save and nothing to weigh.
 */
export function existingLine(existing: ExistingInstance): StatusLine {
  const detail = !existing.passwordChanged
    ? "This server's pack is already in that instance, so open it to play — Geode brings it up to date from the server whenever you press Play. Importing again makes a second, separate instance."
    : existing.certificateChanged
      ? "The server's password has changed since you joined, and this link is for a different certificate than that machine had then. Opening the instance saves both to it, and its updates come from whoever answers there from now on — so open it only if this link came from the admin, who would see the same after reinstalling Geode."
      : "The server's password has changed since you joined. Opening the instance saves the new password to it, so it keeps getting the server's updates.";
  return {
    tone: existing.passwordChanged && existing.certificateChanged ? "warn" : "info",
    title: `Already joined as ${existing.name}`,
    detail,
  };
}

/** A finished import's problems, headed the way the join dialog shows them. */
export function outcomeLine(outcome: JoinOutcome): StatusLine {
  const count = outcome.problems.length;
  return {
    tone: "warn",
    title: `Imported ${outcome.instance.name}, with ${count === 1 ? "a problem" : `${count} problems`}`,
    detail:
      "The instance is made and open behind this dialog, but what is listed here still needs you. Importing again would not fix it: it would make a second instance with the same gaps.",
  };
}

export type Tone = "ok" | "info" | "warn" | "bad";

export interface StatusLine {
  tone: Tone;
  title: string;
  detail: string;
}

/** Somebody else's words — an OS error, a router's — ended with one full stop. */
function sentence(text: string): string {
  const trimmed = text.trim();
  return /[.!?]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}

/** Whether the pack server itself is up. */
export function listenLine(status: ShareStatus): StatusLine {
  if (status.listenError) {
    return {
      tone: "bad",
      title: `Not listening on port ${status.port}`,
      detail: `${sentence(status.listenError)} Nobody can download a pack until this is fixed — if another program already has the port, pick a different one in Settings.`,
    };
  }
  if (status.listening) {
    return {
      tone: "ok",
      title: `Listening on port ${status.port}`,
      detail: "Players' Geode connects here to fetch the pack.",
    };
  }
  return {
    tone: "info",
    title: "Not listening",
    detail: `Geode listens on port ${status.port} while any server is shared.`,
  };
}

type Reachability = NonNullable<ShareStatus["detected"]>["reachability"];

/**
 * An address the router reported that players cannot reach, as what it means
 * for them — or null for a public one, which the caller describes.
 *
 * Shared by automatic setup, where the router also opened `port`, and manual
 * setup, where Geode only asked it for its address and the admin forwards the
 * port on it themselves.
 */
function unreachableLine(
  reachability: Reachability,
  ip: string,
  port: number,
  opened: boolean,
  hasPublicAddress: boolean,
): StatusLine | null {
  const reported = opened
    ? `Your router opened the port, but the address it reports (${ip})`
    : `The address your router reports (${ip})`;
  // With a public address set, links already go somewhere else — a tunnel or a
  // domain — so pointing the admin at that setting would repeat advice they
  // already took.
  const workaround = hasPublicAddress
    ? " Links use your public address instead, which works if it reaches this computer another way, such as a tunnel."
    : " A tunnel such as playit.gg gets around this: put its address under Public address in Settings.";

  switch (reachability) {
    case "public":
      return null;
    case "cgnat":
      return {
        tone: "bad",
        title: "Players cannot reach this computer",
        detail: `${reported} is one your internet provider shares between many customers (carrier-grade NAT), so connections from outside never arrive here.${workaround}`,
      };
    case "doubleNat": {
      const forward = opened
        ? `Forward TCP port ${port} on that one as well`
        : `Forward TCP port ${port} on both routers`;
      // Forwarding alone never makes a link: this router goes on reporting the
      // private address, and Geode cannot see past it to the public one.
      const then = hasPublicAddress
        ? `${forward}, or put the outer one in bridge mode.`
        : `${forward} and put your public IP under Public address in Settings, since Geode cannot see past this router to learn it — or put the outer one in bridge mode.`;
      return {
        tone: "warn",
        title: "Another router is in front of this one",
        detail: `${reported} is a private one, so there is a second router — often your provider's modem — between it and the internet. ${then}${workaround}`,
      };
    }
    case "noIpv4":
      return {
        tone: "bad",
        title: "No public IPv4 address",
        detail: `${reported} is not a public IPv4 address, and your connection has none that players can reach.${workaround}`,
      };
  }
}

/**
 * For a router that answered but would not open the port: forwarding it by
 * hand is not the end of it, because automatic setup reads no address from a
 * router that refused. Manual setup does.
 */
const readsAddressOnceManual =
  " Then choose Set it up myself under Router in Settings, and Geode reads your public address from the router for the link.";

/**
 * What the router said, in terms of whether players can actually connect.
 *
 * Honest rather than hopeful: a router that agreed to open the port has not
 * made this computer reachable if the address it reports is shared with other
 * customers or sits behind another router, and saying "open" there would send
 * an admin off to hand out a link that can never work.
 */
export function routerLine(status: ShareStatus, hasPublicAddress: boolean): StatusLine {
  const port = status.port;
  const mapping = status.mapping;

  switch (mapping.state) {
    case "off": {
      // With automatic setup, off means the router was never asked: its
      // mapping only runs while Geode listens. Calling that manual would send
      // an admin who chose automatic off to forward a port by hand, and away
      // from the listener that is actually what is wrong.
      if (status.mode === "auto") {
        return {
          tone: "info",
          title: "Your router has not been asked yet",
          detail: status.listenError
            ? `Geode asks your router to open port ${port} once it is listening, and it is not — fix that first.`
            : `Geode asks your router to open port ${port} as soon as it is listening.`,
        };
      }
      const detected = status.detected;
      const unreachable =
        detected && unreachableLine(detected.reachability, detected.ip, port, false, hasPublicAddress);
      if (unreachable) return unreachable;
      // No address yet is only the router's answer once it has been asked:
      // before Geode listens it asks nothing, and the first ask takes a moment.
      const address = hasPublicAddress
        ? " Links use your public address from Settings."
        : detected
          ? ` Your router reports ${detected.ip} as its public address, so links use ${detected.ip}:${port}.`
          : !status.listening
            ? " Geode asks your router for its public address once it is listening."
            : status.asking
              ? " Geode is asking your router for its public address."
              : " Your router did not say what its public address is, so links need one under Public address in Settings.";
      return {
        tone: "info",
        title: "Router setup is manual",
        detail: `Geode changes nothing on your router. Forward TCP port ${port} on it to this computer, or point your tunnel at that port.${address}`,
      };
    }
    case "searching":
      return {
        tone: "info",
        title: `Asking your router to open port ${port}…`,
        detail: "This can take a few seconds.",
      };
    case "mapped": {
      const unreachable = unreachableLine(
        mapping.reachability,
        mapping.externalIp,
        mapping.port,
        true,
        hasPublicAddress,
      );
      if (unreachable) return unreachable;
      const where = `${mapping.externalIp}:${mapping.port}`;
      // A router can hand out a different port when the asked-for one is taken.
      const moved =
        mapping.port !== port
          ? ` Your router gave port ${mapping.port}, because ${port} was already taken there.`
          : "";
      return {
        tone: "ok",
        title: `Port ${mapping.port} is open on your router`,
        detail: `Players should be able to connect at ${where}.${moved} If they cannot, check that a firewall on this computer is not blocking Geode.`,
      };
    }
    case "failed":
      switch (mapping.reason) {
        case "noGateway":
          return {
            tone: "warn",
            title: "No router answered",
            detail: `Nothing answered Geode's request to open the port. UPnP may be turned off on your router, or a VPN or firewall on this computer is in the way. Turn UPnP on, or forward TCP port ${port} to this computer by hand.`,
          };
        case "refused":
          return {
            tone: "warn",
            title: "Your router refused",
            detail: `Your router answered but would not open the port — UPnP is on, but restricted. Forward TCP port ${port} to this computer by hand.${readsAddressOnceManual}`,
          };
        case "portInUse":
          return {
            tone: "warn",
            title: `Port ${port} is taken on your router`,
            detail: `Another device on your network already has port ${port} forwarded to it. Pick a different port in Settings, or forward one to this computer by hand.`,
          };
        case "permanentOnly":
          return {
            tone: "warn",
            title: "Your router only opens ports for good",
            detail: `Your router would open the port only permanently, with no expiry. Geode will not ask for that: if Geode ever stopped without closing it, the port would stay open to this computer until someone removed it by hand. Forward TCP port ${port} to this computer yourself instead.${readsAddressOnceManual}`,
          };
        case "other":
          return {
            tone: "warn",
            title: "Your router could not open the port",
            detail: `${mapping.detail ? `It said: ${sentence(mapping.detail)} ` : ""}Forward TCP port ${port} to this computer by hand.${readsAddressOnceManual}`,
          };
      }
  }
  // Unreachable while the union above is covered; a new state from a newer
  // backend should read as unknown rather than crash the page.
  return {
    tone: "info",
    title: "Router status unknown",
    detail: `Forward TCP port ${port} to this computer if players cannot connect.`,
  };
}

/** Why a shared server has no link to copy yet. */
export function linkPendingReason(status: ShareStatus | null, hasPublicAddress: boolean): string {
  const working =
    "Geode is still working out the address players will use. The link appears here as soon as it is known.";
  if (!status) return working;
  // Before anything about addresses: nothing is listening, so any address
  // would be a link to nowhere, and the fix is the port, not Settings'
  // public address.
  if (status.listenError) {
    return `Geode is not listening on port ${status.port}, so a link would lead nowhere. The note on listening, below, says why.`;
  }
  const mapping = status.mapping;
  if (mapping.state === "searching" || hasPublicAddress) return working;
  // Not listening yet, or still asking the router: nothing has been learned
  // to explain.
  if (!status.listening || (status.mode === "manual" && status.asking)) return working;

  // An address the router did report, whether it opened the port or was only
  // asked. Saying it reported nothing would contradict the note below that
  // names it, and "your public IP" is no answer behind carrier-grade NAT.
  const reported =
    mapping.state === "mapped"
      ? { ip: mapping.externalIp, reachability: mapping.reachability, port: mapping.port }
      : mapping.state === "off" && status.mode === "manual" && status.detected
        ? { ...status.detected, port: status.port }
        : null;
  if (reported) {
    const tunnel = "A link needs a tunnel's address instead: put it under Public address in Settings.";
    switch (reported.reachability) {
      case "public":
        return working;
      case "cgnat":
        return `Your router reports ${reported.ip}, an address your internet provider shares between many customers, so Geode will not put it in a link. ${tunnel}`;
      case "noIpv4":
        return `Your router reports ${reported.ip}, which is not a public IPv4 address, so Geode will not put it in a link. ${tunnel}`;
      case "doubleNat":
        return `Your router reports ${reported.ip}, a private address from behind a second router, so Geode will not put it in a link, and cannot see past that router to learn the public one. Once the other router forwards TCP port ${reported.port} too, put your public IP under Public address in Settings — or a tunnel's address.`;
    }
  }

  if (mapping.state === "off") {
    return status.mode === "auto"
      ? working
      : "Your router did not tell Geode its public address, so there is nothing to put in a link yet. Set it under Public address in Settings — your public IP, a domain, or a tunnel's address.";
  }
  if (mapping.state === "failed" && ["refused", "permanentOnly", "other"].includes(mapping.reason)) {
    return "Your router would not open the port, so there is nothing to put in a link yet. Once the port is forwarded by hand, choose Set it up myself under Router in Settings and the link appears on its own — or set the address under Public address in Settings: your public IP, a domain, or a tunnel's address.";
  }
  return "Your router did not open the port or report an address, so there is nothing to put in a link yet. Once the port is forwarded by hand, set the address under Public address in Settings — your public IP, a domain, or a tunnel's address.";
}

/**
 * What the Share tab says about a server whose sharing is stopped.
 *
 * Sharing again brings back the share as it was, so the note promises what
 * that is and nothing more. Links work again only if they carry the password
 * the server has now, which is not so once it was replaced while stopped; and
 * a config folder that was being sent is sent again, which an admin who has
 * stopped sharing may not expect.
 */
export function stoppedLine(includeConfig: boolean): StatusLine {
  const config = includeConfig ? " Sharing again also sends the config folder, as it did before." : "";
  return {
    tone: "info",
    title: "Sharing is off",
    detail: `This server keeps its password while it is not shared. Sharing again uses the same password, so links you sent with it work again.${config} If the password reached someone it should not have, replace it first.`,
  };
}

/**
 * What to tell an admin who has just replaced the password. A server whose
 * sharing is stopped answers nobody, so its new link is no use to players
 * until it is shared again.
 */
export function replacedPasswordBody(shared: boolean): string {
  return shared
    ? "Send players the new link or password."
    : "Share the server again, then send players the new link or password.";
}

/**
 * What an admin reading out their address and password has to tell a player.
 *
 * The address is the one links carry, port and all, but a player who types it
 * without the port is looked up on 25590 whatever this machine listens on —
 * and told the server could not be reached.
 */
export function readOutHint(address: string | null): string {
  if (!address) {
    return "For reading out, with this computer's address once Geode knows it: a player types both under Join a server.";
  }
  if (/^https:\/\//i.test(address)) {
    return "For reading out when a link will not do: a player types both under Join a server, https:// included. The link saves them the typing, and is checked the same way.";
  }
  const port = Number(/:(\d+)$/.exec(address)?.[1]);
  const typed =
    port === DEFAULT_SHARE_PORT
      ? "a player types both under Join a server, and can leave the port off."
      : `a player types both under Join a server, port included — without it, their Geode looks on ${DEFAULT_SHARE_PORT} instead.`;
  return `For reading out when a link will not do: ${typed} The link is better: their Geode can check from the very first connection that it has reached this computer.`;
}

/**
 * How the connection to a server was checked, for the join confirmation.
 *
 * `typed` is whether the player typed the address rather than opening a link.
 * A typed address is only ever pinned by the fingerprint Geode remembered
 * from an instance already joined through it, and crediting that to a link
 * would describe a check the player never had.
 */
export function verificationLine(verified: JoinPreview["verified"], typed: boolean): StatusLine {
  switch (verified) {
    case "pinned":
      return typed
        ? {
            tone: "ok",
            title: "Verified against your earlier join",
            detail:
              "The server's certificate matches the one Geode remembered when you joined this address before, so this is the same computer.",
          }
        : {
            tone: "ok",
            title: "Verified against the link",
            detail:
              "The server's certificate matches the fingerprint in the link, so this is the computer the link was made for.",
          };
    case "domain":
      return {
        tone: "ok",
        title: "Verified by the site's certificate",
        detail:
          "The address proved who it is with a certificate from a certificate authority — the same check a browser makes.",
      };
    case "firstUse":
      // Remembered by the instance an import makes, so the promise is about
      // importing: a lookup alone keeps nothing, and neither does deleting the
      // instance later.
      return {
        tone: "warn",
        title: "First connection to this address",
        detail:
          "There was nothing to check this server against, so Geode trusts the certificate it saw. Importing remembers it with the instance, and typing this address again is checked against it: a different certificate is refused. A link from the server's admin is checked from the very first connection, so prefer one when you can.",
      };
  }
}
