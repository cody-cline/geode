// What the sharing screens say, kept apart from the screens so it can be
// tested on its own. Nothing here may import a value — only types — because
// the tests load this file straight into Node, which has no bundler to resolve
// the app's extensionless imports.

import type {
  DriveStatus,
  ExistingInstance,
  ExportReport,
  Instance,
  JoinInput,
  JoinOutcome,
  JoinPreview,
  PackArrival,
  PackLinkView,
  PublicCheck,
  PackInfo,
  ServerShareView,
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

/**
 * What importing will add to the player's multiplayer list, for the join
 * dialog — or null when the server named nothing to add.
 *
 * Worth saying before they press Import rather than after: it is the one thing
 * an import writes outside the instance's own mods, and it is the answer to
 * "I have the pack, now where do I actually connect?". A server on a Geode too
 * old to send a listing, or an admin who set none, says nothing here, and the
 * player gets what they got before: the pack, and an address to ask for.
 */
export function listingLine(info: PackInfo): StatusLine | null {
  if (!info.listing) return null;
  // Opening straight into the server is quick play, which the launch emits
  // only where the version's own manifest declares the feature. Promising it
  // on a version that has never had it would be promising a title screen.
  const andPlay = opensStraightIntoAServer(info.mcVersion)
    ? ", and points Play straight at it."
    : `. Play cannot open straight into a server before Minecraft 1.20, so on ${info.mcVersion} pick it from the Multiplayer screen.`;
  return {
    tone: "info",
    title: `${info.listing.name} will be added to your server list`,
    detail: `Importing adds ${info.listing.name} at ${info.listing.address} to this instance's multiplayer list${andPlay} Your own servers are left alone. Close Minecraft first if it is open — the game rewrites that list when it closes.`,
  };
}

/**
 * Whether this version can be launched straight into a server.
 *
 * The launch asks the version's own manifest for the `is_quick_play_multiplayer`
 * feature rather than reading the number, so this is the dialog's nearest
 * answer to the same question: 1.20 is where Mojang added it, and a version
 * this cannot read as a release — a snapshot — is not promised it either.
 */
function opensStraightIntoAServer(mcVersion: string): boolean {
  const release = /^1\.(\d+)(\D|$)/.exec(mcVersion.trim());
  return release ? Number(release[1]) >= 20 : false;
}

/**
 * What the admin needs to know about the two addresses, for the Share tab.
 *
 * They are easy to confuse and the cost of confusing them is a server list
 * full of rows that cannot connect: packs come down Geode's own port, which is
 * the same for every server this machine shares, while Minecraft connects to
 * this server's own `server-port`. So the hint names both and says which is
 * which, and says plainly when Geode could not work the game address out —
 * which is every `https://` share address, since a tunnel in front of the pack
 * port says nothing about where the game's port is.
 */
export function gameAddressHint(share: ServerShareView): string {
  const pack = share.packAddress;
  const packSentence = pack
    ? `Packs come down ${pack}, which is Geode's port and the same for every server you share.`
    : "Packs come down Geode's own port, which is the same for every server you share.";

  if (share.listing) {
    return `${packSentence} Players connect to ${share.listing.address} to play. Change it here if you move the server or forward a different port; players get the change the next time they press Play, with nothing downloaded.`;
  }
  if (share.suggested.address) {
    return `${packSentence} Geode has filled in this server's own server-port for the game address — check it is the one you forwarded, then save it.`;
  }
  return `${packSentence} Geode cannot work out the game address from that, so type the one players put into Minecraft's Add Server.`;
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
export function sentence(text: string): string {
  const trimmed = text.trim();
  return /[.!?]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}

/**
 * Windows asks before it lets a program take connections from outside, and the
 * router never hears that answer: a prompt that was dismissed leaves the port
 * open on the router with nothing reachable behind it.
 *
 * Said on the listening line alone, which is what Windows prompts about: the
 * router line sits directly beneath it on the same panel, so a copy there
 * would print the same forty words twice.
 */
const windowsFirewall =
  " On Windows, allow Geode through the firewall when it asks the first time you share a server: refused, the port still looks open on your router while every player times out.";

/**
 * Changing the port is the one fix that costs something, so it is never the
 * first thing said: every link already handed out carries the old one.
 */
const portCosts =
  " Changing the port in Settings breaks every link you have already sent, so do that only if you are ready to send everyone a new one.";

/** Whether the pack server itself is up, and what is being done about it. */
export function listenLine(status: ShareStatus): StatusLine {
  if (status.listening) {
    return {
      tone: "ok",
      title: `Listening on port ${status.port}`,
      detail: `Players' Geode connects here to fetch the pack.${windowsFirewall}`,
    };
  }
  const retry = status.retry;
  const why = status.listenError ? `${sentence(status.listenError)} ` : "";
  // Still trying is not broken: the usual cause is another Geode that has just
  // closed and is still giving the port back, which takes a few seconds.
  if (retry && !retry.needsYou) {
    // "Within", not "in": the countdown is worked out when the status is sent
    // and the status is only sent once an attempt, so the number stands still
    // for the whole wait. A ceiling it never passes stays true as it does.
    const when =
      retry.nextInSeconds <= 1 ? "in a moment" : `within ${retry.nextInSeconds} seconds`;
    return {
      tone: "warn",
      title: `Waiting for port ${status.port}`,
      detail: `${why}Geode is trying again ${when} — attempt ${retry.attempts + 1}. This usually clears on its own within seconds, most often because another Geode has just closed and is still giving the port back. Leave the port as it is while it tries.${portCosts}`,
    };
  }
  if (retry) {
    return {
      tone: "bad",
      title: `Still not listening on port ${status.port}`,
      detail: `${why}Geode has been trying for over a minute — ${retry.attempts} attempts — and goes on trying, but something is holding the port: close the program that has it, or let Geode through this computer's firewall. Nobody can download a pack until then.${portCosts}`,
    };
  }
  if (status.listenError) {
    return {
      tone: "bad",
      title: `Not listening on port ${status.port}`,
      detail: `${sentence(status.listenError)} Nobody can download a pack until this is fixed — if another program already has the port, close it, or pick a different port in Settings.${portCosts}`,
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
        // Bridging the outer router changes nothing here until Geode asks
        // again, which it does on its own — so saying when saves an admin who
        // has just done it from going back through Settings to no effect.
        detail: `${reported} is a private one, so there is a second router — often your provider's modem — between it and the internet. ${then} Geode asks your router again within half an hour, so a change out there shows up here on its own.${workaround}`,
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
 * Said in every failure because none of them is the end of it: the mapper tries
 * again ten seconds later and at most a minute apart after that, and the page
 * follows on its own. Without it an admin who has just switched UPnP on has no
 * sign that anything is happening, and restarts Geode or changes the port —
 * the one change that breaks links already handed out.
 *
 * Kept ahead of the switch-to-manual aside below, because manual setup stops
 * the mapper: last, the minute would read as a promise about the setup the
 * sentence before it advises, where the router is asked again every half hour.
 */
const keepsAsking =
  " Geode keeps asking on its own, so a change on your router is picked up within a minute.";

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
        // A listener Geode is still trying for is Geode's to fix, not the
        // admin's: the listening note sits directly above this one and says
        // to leave the port alone while it tries, so "fix that first" here
        // would give two answers for the same second.
        const trying = Boolean(status.retry && !status.retry.needsYou);
        return {
          tone: "info",
          title: "Your router has not been asked yet",
          detail:
            status.listenError && !trying
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
      // The pack port and nothing else: read as the address players put into
      // Minecraft, this would send an admin off to hand out a port no game
      // server listens on.
      return {
        tone: "ok",
        title: `Port ${mapping.port} is open on your router`,
        detail: `Players' Geode can download this server's pack at ${where}. If they cannot, check that a firewall on this computer is not blocking Geode.`,
      };
    }
    case "failed": {
      // Links already use the address from Settings, and always would: manual
      // detection could not change theirs, so pointing them at it would ask
      // them to expose a port for nothing.
      const thenManual = hasPublicAddress ? "" : readsAddressOnceManual;
      switch (mapping.reason) {
        case "noGateway":
          return {
            tone: "warn",
            title: "No router answered",
            detail: `Nothing answered Geode's request to open the port. UPnP may be turned off on your router, or a VPN or firewall on this computer is in the way. Turn UPnP on, or forward TCP port ${port} to this computer by hand.${keepsAsking}`,
          };
        case "refused":
          return {
            tone: "warn",
            title: "Your router refused",
            detail: `Your router answered but would not open the port — UPnP is on, but restricted. Forward TCP port ${port} to this computer by hand.${keepsAsking}${thenManual}`,
          };
        case "portInUse":
          return {
            tone: "warn",
            title: `Port ${port} is taken on your router`,
            detail: `Another device on your network already has port ${port} forwarded to it. Pick a different port in Settings, or forward one to this computer by hand.${keepsAsking}`,
          };
        case "permanentOnly":
          return {
            tone: "warn",
            title: "Your router only opens ports for good",
            detail: `Your router would open the port only permanently, with no expiry. Geode will not ask for that: if Geode ever stopped without closing it, the port would stay open to this computer until someone removed it by hand. Forward TCP port ${port} to this computer yourself instead.${keepsAsking}${thenManual}`,
          };
        case "other":
          return {
            tone: "warn",
            title: "Your router could not open the port",
            detail: `${mapping.detail ? `It said: ${sentence(mapping.detail)} ` : ""}Forward TCP port ${port} to this computer by hand.${keepsAsking}${thenManual}`,
          };
      }
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

/**
 * An address the router reported, whether it opened the port or was only asked,
 * or null before it has said one. Shared so the missing link and the read-out
 * hint agree about whether an address is on its way or will never come.
 */
function reportedAddress(
  status: ShareStatus,
): { ip: string; reachability: Reachability; port: number } | null {
  const mapping = status.mapping;
  if (mapping.state === "mapped") {
    return { ip: mapping.externalIp, reachability: mapping.reachability, port: mapping.port };
  }
  if (mapping.state === "off" && status.mode === "manual" && status.detected) {
    return { ...status.detected, port: status.port };
  }
  return null;
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
    // While it is still trying, the link is late rather than impossible, and
    // the note below says what it is waiting for.
    return status.retry && !status.retry.needsYou
      ? `Geode is not listening on port ${status.port} yet, so a link would lead nowhere. It is trying again — the note on listening, below, says what it is waiting for.`
      : `Geode is not listening on port ${status.port}, so a link would lead nowhere. The note on listening, below, says why.`;
  }
  const mapping = status.mapping;
  if (mapping.state === "searching" || hasPublicAddress) return working;
  // Not listening yet, or still asking the router: nothing has been learned
  // to explain.
  if (!status.listening || (status.mode === "manual" && status.asking)) return working;

  // Saying the router reported nothing would contradict the note below that
  // names it, and "your public IP" is no answer behind carrier-grade NAT.
  const reported = reportedAddress(status);
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
export function readOutHint(status: ShareStatus | null): string {
  const address = status?.address ?? null;
  if (!address) {
    // Behind carrier-grade NAT, a second router or an IPv6-only connection the
    // router has already answered, and its answer is not one players can
    // reach: there is no address on the way to promise. What to do about it is
    // already on the missing-link note above and the router note below, so this
    // one keeps to what a player does with what is here.
    const reported = status && reportedAddress(status);
    return reported && reported.reachability !== "public"
      ? "For reading out: a player types the address and password under Join a server, but the address your router reports is not one they can reach, so there is none to read out."
      : "For reading out, with this computer's address once Geode knows it: a player types both under Join a server.";
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
 * What this computer's own address on its network is for, beside the password.
 *
 * It carries the one thing an admin has to know before they try their own
 * link: a connection to the public address from inside the network has to
 * leave the router and come straight back in through it, and plenty of home
 * routers refuse that. So the admin's own test, from the very machine that is
 * sharing, can come back "could not be reached" while every player outside
 * gets through — which reads as a broken port forward when nothing is broken.
 * Said here because this is the address that does work from here, so the
 * sentence arrives beside what to do about it.
 *
 * With no public address yet there is no link to try, so nothing is said about
 * trying one.
 */
export function localAddressHint(status: ShareStatus | null): string {
  const here =
    "For players in the same house or on the same Wi-Fi, and for trying this yourself: this address and the password, or the link beside it.";
  const address = status?.address ?? null;
  if (!address) return here;
  const why = /^https:\/\//i.test(address)
    ? "the link above goes out to your domain or tunnel and back, and not every setup sends that back in to your own network"
    : "the link above has to go out to your router and come back in, which many routers will not do";
  return `${here} Your own link can fail when you open it from this network, this computer included, because ${why}. That is the router, not the share: players elsewhere are unaffected.`;
}

/**
 * Whether the links already sent point at an address this computer no longer
 * has. `lastLinkAddress` comes from the backend and is null unless the server
 * is shared and a link has actually been taken.
 *
 * The one comparison behind both the sentence on the Share tab and the badge on
 * the server lists, so the two can never disagree about whether a link has gone
 * stale. True only once there is something to compare: an admin who has never
 * taken a link has sent nobody one, and before Geode knows an address there is
 * no new link to point at instead.
 */
export function linksPointAtOldAddress(
  status: ShareStatus | null,
  lastLinkAddress: string | null,
): boolean {
  const address = status?.address ?? null;
  return Boolean(address && lastLinkAddress && address !== lastLinkAddress);
}

/**
 * What the badge on a server's row and card says, for hovering and for a
 * screen reader. Here rather than in the two lists, so one warning cannot come
 * to be worded two ways.
 */
export const OLD_ADDRESS_LABEL = "The links you sent point at an old address";

/**
 * The sentence under the link when the address has moved on.
 *
 * Nothing here is said about the instances players already made — a link at a
 * new address does not find them, and this note is not the place to promise it
 * does.
 */
export function addressChangedLine(
  status: ShareStatus | null,
  lastLinkAddress: string | null,
): StatusLine | null {
  if (!linksPointAtOldAddress(status, lastLinkAddress)) return null;
  return {
    tone: "warn",
    title: "This computer's address has changed",
    detail: `The links you have already sent point at ${lastLinkAddress} and no longer work. The link above is the one to send now.`,
  };
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
          "There was nothing to check this server against, so Geode trusts the certificate it saw. Importing remembers it with the instance, and typing this address again is checked against it for as long as an instance joined from it is still there: a different certificate is refused. A link from the server's admin is checked from the very first connection, so prefer one when you can.",
      };
  }
}

// ---------------------------------------------------------------------------
// Sharing a pack through Google Drive
// ---------------------------------------------------------------------------

/**
 * Which kind of `geode://` link this is, or null for neither.
 *
 * One handler, one parked slot and one event carry both kinds, so this is the
 * one place that chooses which dialog opens. Only the host is read: the real
 * parsing is strict and happens in Rust, and a link the webview thinks is a
 * pack link is still refused there if it is not one.
 */
export function linkKind(raw: string): "join" | "pack" | null {
  const front = raw.trim().split("?")[0].replace(/\/$/, "").toLowerCase();
  if (front === "geode://join") return "join";
  if (front === "geode://pack") return "pack";
  return null;
}

/** One pack dialog's worth of intent: the link it was opened with. */
export interface PackRequest {
  /** Unique for the session, so a replaced dialog remounts from scratch. */
  seq: number;
  link: string;
}

/**
 * The pack dialog after a link arrives.
 *
 * The same rule as `nextJoin`: a different link replaces the dialog, the same
 * link again — the parked copy and the event both delivering it, or StrictMode
 * running an effect twice — leaves it alone, so a download in progress is not
 * thrown away and started over.
 */
export function nextPack(
  current: PackRequest | null,
  link: string,
  seq: number,
): PackRequest {
  if (current && link === current.link) return current;
  return { seq, link };
}

/**
 * Where the pack dialog is up to.
 *
 * Nothing is fetched until the link has been read in Rust, and nothing is
 * imported until the bytes have been downloaded and their hash has matched —
 * so each step is named by what has actually come back, not by what was
 * pressed. Once there is an instance there is nothing left to offer:
 * importing again would make a second one.
 */
export function packStep(
  view: PackLinkView | null,
  arrival: PackArrival | null,
  instance: Instance | null,
): "look" | "download" | "import" | "done" {
  if (instance) return "done";
  if (arrival) return "import";
  return view ? "download" : "look";
}

/** Where a pack link's bytes come from, in words. */
export function sourceName(view: PackLinkView): string {
  return view.source === "drive" ? "Google Drive" : view.host;
}

/** What the receiving dialog says before anything is downloaded. */
export function arrivalLine(view: PackLinkView): StatusLine {
  return {
    tone: "info",
    title: `A pack from ${sourceName(view)}`,
    detail:
      "Geode will download it to see what is in it, check it against the fingerprint in the link, and show you before anything is installed. Nothing is imported until you say so.",
  };
}

/**
 * What an export did with the instance's content, for the moment before it is
 * published.
 *
 * A pack that carries somebody else's jar as bytes is about to be put on a
 * public URL, which is a redistribution decision and not just a transfer. So
 * the counts the export actually produced are shown — not a promise about what
 * usually happens — and the files that could not go in at all are named while
 * there are few enough to read.
 */
export function publishSummary(report: ExportReport): string[] {
  const lines: string[] = [];
  const embedded =
    report.embeddedUnknown + report.embeddedElsewhere + report.embeddedTurnedOff;
  if (report.referenced > 0) {
    lines.push(
      `${report.referenced} ${report.referenced === 1 ? "file travels" : "files travel"} as a reference, fetched by whoever installs the pack.`,
    );
  }
  if (embedded > 0) {
    const why: string[] = [];
    if (report.embeddedUnknown > 0) {
      why.push(`${report.embeddedUnknown} because no provider could name ${report.embeddedUnknown === 1 ? "it" : "them"}`);
    }
    if (report.embeddedElsewhere > 0) {
      why.push(`${report.embeddedElsewhere} because this format cannot carry a reference to ${report.embeddedElsewhere === 1 ? "it" : "them"}`);
    }
    if (report.embeddedTurnedOff > 0) {
      why.push(`${report.embeddedTurnedOff} because ${report.embeddedTurnedOff === 1 ? "it is" : "they are"} turned off`);
    }
    lines.push(
      `${embedded} ${embedded === 1 ? "file travels" : "files travel"} as bytes — ${why.join(", ")}. Uploading publishes ${embedded === 1 ? "that copy" : "those copies"}.`,
    );
  }
  if (report.unnameable.length > 0) {
    const named =
      report.unnameable.length <= 5
        ? `: ${report.unnameable.join(", ")}`
        : "";
    lines.push(
      `${report.unnameable.length} ${report.unnameable.length === 1 ? "file" : "files"} could not go in at all${named}.`,
    );
  }
  for (const trouble of report.trouble) lines.push(sentence(trouble));
  if (lines.length === 0) lines.push("This pack carries no mods at all.");
  return lines;
}

/** Said once, where the link is made, because it cannot be taken back. */
export const PUBLIC_LINK_WARNING =
  "Uploading publishes this file. Anyone with the link can download it, and that cannot be undone for anyone who already has — delete the file in your Drive to stop the link working.";

/**
 * What the sharing machine found when it fetched its own public link.
 *
 * Drive's download endpoint is undocumented and interposes a virus-scan
 * warning page above some size nobody has published, so this is the one thing
 * in this transport that can only be found out by trying it. Better the sharer
 * finds out than the person they sent it to.
 */
export function publicLinkLine(check: PublicCheck): StatusLine {
  switch (check) {
    case "served":
      return {
        tone: "ok",
        title: "Checked — the link serves the pack",
        detail:
          "Geode fetched the link from this computer with nobody signed in, which is what whoever you send it to will get.",
      };
    case "webPage":
      return {
        tone: "warn",
        title: "Google answered with a web page for this link",
        detail:
          "Anyone you send it to will have to open the Drive link in a browser, download the file, and use Add instance → Import a pack. This happens with large packs; a smaller one usually downloads straight away.",
      };
    case "unknown":
      return {
        tone: "info",
        title: "The link could not be checked from here",
        detail:
          "The pack is uploaded and shared. Geode could not fetch the link itself to see what a stranger gets, so try the link yourself before sending it on.",
      };
  }
}

/** What a sharer pastes wherever they are sending it. */
export interface PackMessage {
  name: string;
  link: string;
  fileUrl: string;
  /** Already formatted: this file may not import `fileSize`, only types. */
  size: string;
}

/**
 * The message, geode:// link first.
 *
 * The plain Drive link is in the body so somebody without Geode can still get
 * the file, and it is the link Drive itself returned rather than one Geode
 * assembled. The last line is the one place the public-link fact is stated, and
 * it sits in the message as well as in the dialog so it travels with the thing
 * being shared.
 */
export function packShareMessage(message: PackMessage): string {
  return [
    `${message.name} — a Minecraft pack for Geode`,
    "",
    "Open this in Geode to install it:",
    message.link,
    "",
    `Or download the file (${message.size}):`,
    message.fileUrl,
    "",
    "It is a .geodepack — a .mrpack any launcher can open.",
    "Anyone with these links can download it.",
  ].join("\n");
}

/** The line Settings shows for the Drive connection. */
export function driveLine(status: DriveStatus): StatusLine {
  if (!status.configured) {
    return {
      tone: "info",
      title: "Not configured",
      detail:
        "Sharing a pack through Drive needs your own Google client ID. Offline sharing works without one: export a .geodepack and hand it over.",
    };
  }
  if (status.needsReconnect) {
    return {
      tone: "warn",
      title: "Reconnect needed",
      detail:
        "Google will not accept this connection again. If your OAuth client is still in Testing, Google expires every authorization after seven days — press Publish app in Google's Audience screen and connect again.",
    };
  }
  if (!status.connected) {
    return {
      tone: "info",
      title: "Not connected",
      detail:
        "Press Connect to sign in to Google in your browser. Geode only ever sees the files it creates in your Drive.",
    };
  }
  return {
    tone: "ok",
    title: status.account ? `Connected as ${status.account}` : "Connected",
    detail: status.folderName
      ? `Packs you share go into ${status.folderName} in your Drive. Geode cannot see anything else there.`
      : "Geode cannot see anything in your Drive but the files it created there.",
  };
}
