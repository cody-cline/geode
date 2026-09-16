// Run with `node --test src/lib/share.test.mjs` (Node 22.18 or newer, which
// strips the types from share.ts itself). Plain JavaScript on purpose: the
// project has no test runner and no Node typings, and TypeScript leaves .mjs
// files out of its check.

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  existingLine,
  joinInput,
  joinStep,
  linkPendingReason,
  listenLine,
  nextJoin,
  outcomeLine,
  parsePort,
  readOutHint,
  replacedPasswordBody,
  routerLine,
  stoppedLine,
  verificationLine,
} from "./share.ts";

function status(mapping, extra = {}) {
  return {
    mode: "auto",
    detected: null,
    asking: false,
    listening: true,
    port: 25590,
    listenError: null,
    mapping,
    address: null,
    fingerprint: "ab".repeat(32),
    ...extra,
  };
}

/** Manual router setup, which is the only way to be listening with the mapping off. */
const manual = (extra = {}) => status({ state: "off" }, { mode: "manual", ...extra });

const mapped = (reachability, extra = {}) => ({
  state: "mapped",
  externalIp: "203.0.113.7",
  port: 25590,
  reachability,
  ...extra,
});

test("a port must be a whole number a user can listen on", () => {
  assert.equal(parsePort("25590"), 25590);
  assert.equal(parsePort(" 25590 "), 25590);
  assert.equal(parsePort("1024"), 1024);
  assert.equal(parsePort("65535"), 65535);
  for (const bad of ["1023", "80", "0", "65536", "99999", "-1", "25590.5", "1e4", "0x6000", "", "  ", "port"]) {
    assert.equal(parsePort(bad), null, `${JSON.stringify(bad)} should be refused`);
  }
});

test("a link is sent as a link, trimmed", () => {
  const form = { mode: "link", link: "  geode://join?addr=a:1&code=X&pin=00  ", address: "", code: "" };
  assert.deepEqual(joinInput(form), { link: "geode://join?addr=a:1&code=X&pin=00" });
  assert.equal(joinInput({ ...form, link: "   " }), null);
});

test("an address needs a code before it can be looked up", () => {
  const form = { mode: "address", link: "", address: " play.example.com ", code: " GEODE-AAAAA-BBBBB " };
  assert.deepEqual(joinInput(form), { address: "play.example.com", code: "GEODE-AAAAA-BBBBB" });
  assert.equal(joinInput({ ...form, code: "" }), null);
  assert.equal(joinInput({ ...form, address: "" }), null);
});

test("a link pasted into the address box keeps its fingerprint", () => {
  const link = "GEODE://join?addr=203.0.113.7:25590&code=X&pin=ab";
  assert.deepEqual(joinInput({ mode: "address", link: "", address: link, code: "" }), { link });
});

test("the same link twice leaves the open dialog alone", () => {
  const open = nextJoin(null, "geode://join?code=A", 1);
  assert.deepEqual(open, { seq: 1, link: "geode://join?code=A" });
  assert.equal(nextJoin(open, "geode://join?code=A", 2), open);
});

test("a different link replaces the open dialog", () => {
  const open = nextJoin(null, "geode://join?code=A", 1);
  const next = nextJoin(open, "geode://join?code=B", 2);
  assert.notEqual(next, open);
  assert.deepEqual(next, { seq: 2, link: "geode://join?code=B" });
});

test("asking to join by hand does not wipe a dialog already open", () => {
  const open = nextJoin(null, "geode://join?code=A", 1);
  assert.equal(nextJoin(open, null, 2), open);
  assert.deepEqual(nextJoin(null, null, 3), { seq: 3, link: null });
});

/** What a lookup shows, from a server this player may already have joined. */
const preview = (existing = null) => ({
  info: { name: "Survival", mcVersion: "1.21.1", loader: "fabric", loaderVersion: "0.16.9", mods: 3, serverKind: "fabric" },
  address: "203.0.113.7:25590",
  verified: "pinned",
  link: { address: { kind: "direct", host: "203.0.113.7", port: 25590 }, code: "GEODE-AAAAA-BBBBB", pin: "ab".repeat(32) },
  existing,
});

const outcome = (problems) => ({ instance: { id: "survival", name: "Survival" }, problems });

test("an import that made an instance never offers to import again", () => {
  // Problems and all: a second import would be a second instance, not a fix.
  const partial = outcome(["1 file(s) in this server's pack have to be fetched by hand: create-cf.jar"]);
  assert.equal(joinStep(preview(), partial), "done");
  assert.equal(joinStep(preview({ id: "survival", name: "Survival" }), partial), "done");
  assert.equal(joinStep(null, partial), "done");
});

test("a server already joined offers its instance before another import", () => {
  assert.equal(joinStep(preview({ id: "survival", name: "Survival" }), null), "openExisting");
  assert.equal(joinStep(preview(), null), "import");
  assert.equal(joinStep(null, null), "lookUp");
});

/** An instance a lookup matched, with the two facts that decide what is said about it. */
const already = (passwordChanged, certificateChanged) => ({
  id: "survival",
  name: "Survival",
  passwordChanged,
  certificateChanged,
});

test("an instance offered instead of a second import says what opening it does", () => {
  const same = existingLine(already(false, false));
  assert.equal(same.tone, "info");
  assert.equal(same.title, "Already joined as Survival");
  assert.match(same.detail, /already in that instance/);

  // A new password to save, from the machine the instance already knows.
  const newPassword = existingLine(already(true, false));
  assert.equal(newPassword.tone, "info");
  assert.match(newPassword.detail, /password has changed/);
  assert.doesNotMatch(newPassword.detail, /certificate/);

  // And from a machine holding a certificate it has never seen: the same
  // offer, with the fact that tells an admin's reinstall from someone else
  // answering at that address.
  const stranger = existingLine(already(true, true));
  assert.equal(stranger.tone, "warn");
  assert.match(stranger.detail, /different certificate/);
  assert.match(stranger.detail, /only if this link came from the admin/);

  // A certificate is worth nothing to weigh where opening saves no link.
  assert.deepEqual(existingLine(already(false, true)), same);
});

test("a finished import's problems are counted, and not offered as a retry", () => {
  const one = outcomeLine(outcome(["create-cf.jar needs your CurseForge API key"]));
  assert.equal(one.tone, "warn");
  assert.match(one.title, /^Imported Survival, with a problem$/);
  assert.match(outcomeLine(outcome(["a", "b"])).title, /with 2 problems$/);
  assert.match(one.detail, /second instance/);
});

test("a public mapping is the only one that promises a connection", () => {
  const promise = /should be able to connect/i;
  const ok = routerLine(status(mapped("public")), false);
  assert.equal(ok.tone, "ok");
  assert.match(ok.detail, promise);
  assert.match(ok.detail, /203\.0\.113\.7:25590/);

  for (const reachability of ["cgnat", "doubleNat", "noIpv4"]) {
    const line = routerLine(status(mapped(reachability)), false);
    assert.notEqual(line.tone, "ok", reachability);
    assert.doesNotMatch(line.detail, promise, reachability);
    assert.doesNotMatch(line.title, /open on your router/i, reachability);
  }
});

test("carrier-grade NAT says players cannot connect and what to do instead", () => {
  const line = routerLine(status(mapped("cgnat")), false);
  assert.equal(line.tone, "bad");
  assert.match(line.title, /cannot reach/i);
  assert.match(line.detail, /tunnel/i);
  assert.match(line.detail, /Public address/);

  // Already set: say it is in use rather than send them to set it again.
  const set = routerLine(status(mapped("cgnat")), true);
  assert.doesNotMatch(set.detail, /put its address under Public address/);
  assert.match(set.detail, /public address instead/i);
});

test("double NAT names the other router and the port to forward on it", () => {
  const line = routerLine(status(mapped("doubleNat", { externalIp: "192.168.1.20" })), false);
  assert.match(line.title, /another router/i);
  assert.match(line.detail, /192\.168\.1\.20/);
  assert.match(line.detail, /25590/);
});

test("double NAT does not promise that forwarding alone makes a link", () => {
  // The inner router goes on reporting its private address after the outer one
  // forwards the port, so no link appears until the public IP is set by hand.
  const line = routerLine(status(mapped("doubleNat", { externalIp: "192.168.1.20" })), false);
  assert.match(line.detail, /public IP under Public address in Settings/);
  assert.match(line.detail, /bridge mode/);

  const set = routerLine(status(mapped("doubleNat", { externalIp: "192.168.1.20" })), true);
  assert.doesNotMatch(set.detail, /put your public IP/);
  assert.match(set.detail, /bridge mode/);
});

test("a router that reported an unreachable address is not said to have reported none", () => {
  const cases = [
    ["cgnat", "100.64.1.2"],
    ["doubleNat", "192.168.1.2"],
    ["noIpv4", "0.0.0.0"],
  ];
  for (const [reachability, externalIp] of cases) {
    const reason = linkPendingReason(status(mapped(reachability, { externalIp })), false);
    assert.doesNotMatch(reason, /did not report/, reachability);
    assert.ok(reason.includes(externalIp), `${reachability} names ${externalIp}`);
    assert.match(reason, /will not put it in a link/, reachability);
    assert.match(reason, /Public address in Settings/, reachability);
  }

  // Behind carrier-grade NAT or without IPv4 there is no public IP of your own
  // to type in; only a tunnel gets a link.
  for (const reachability of ["cgnat", "noIpv4"]) {
    const reason = linkPendingReason(status(mapped(reachability)), false);
    assert.doesNotMatch(reason, /your public IP/, reachability);
    assert.match(reason, /tunnel/, reachability);
  }

  // Behind a second router the public IP does work, once that router forwards.
  const doubled = linkPendingReason(status(mapped("doubleNat", { externalIp: "192.168.1.2", port: 40123 })), false);
  assert.match(doubled, /your public IP/);
  assert.match(doubled, /40123/);

  // A public mapping still waits for the link rather than inventing a reason.
  assert.match(linkPendingReason(status(mapped("public")), false), /still working out/);
});

test("a router that picked another port says so", () => {
  const line = routerLine(status(mapped("public", { port: 40123 })), false);
  assert.match(line.detail, /40123/);
  assert.match(line.detail, /already taken/);
  const same = routerLine(status(mapped("public")), false);
  assert.doesNotMatch(same.detail, /already taken/);
});

test("every failure and manual setup shows the port to forward by hand", () => {
  const port = 31337;
  const cases = [
    manual({ port }),
    status({ state: "failed", reason: "noGateway" }, { port }),
    status({ state: "failed", reason: "refused" }, { port }),
    status({ state: "failed", reason: "portInUse" }, { port }),
    status({ state: "failed", reason: "permanentOnly" }, { port }),
    status({ state: "failed", reason: "other", detail: "ConflictInMappingEntry" }, { port }),
  ];
  for (const each of cases) {
    const line = routerLine(each, false);
    assert.match(line.detail, /31337/, JSON.stringify(each.mapping));
    assert.notEqual(line.tone, "ok", JSON.stringify(each.mapping));
  }
});

test("a port already in use with automatic setup is not called manual", () => {
  // What the backend reports when the listener could not start: no mapper ran,
  // so the mapping is off even though the admin chose automatic.
  const taken = status({ state: "off" }, { listening: false, listenError: "Address already in use" });
  const router = routerLine(taken, false);
  assert.doesNotMatch(router.title, /manual/i);
  assert.doesNotMatch(router.detail, /Forward TCP port/);
  assert.match(router.title, /not been asked/);

  const pending = linkPendingReason(taken, false);
  assert.doesNotMatch(pending, /manual/i);
  assert.doesNotMatch(pending, /Public address in Settings/);
  assert.match(pending, /not listening on port 25590/);

  // Not a reason to hand out a public address either: it is still not listening.
  assert.doesNotMatch(linkPendingReason(taken, true), /still working out/);
  // A moment before it starts listening is not manual setup either: the router
  // has not been asked, so it has not failed to say anything.
  assert.match(linkPendingReason(status({ state: "off" }, { listening: false }), false), /still working out/);

  // Manual setup that cannot listen says so too, rather than blaming the address.
  assert.match(linkPendingReason(manual({ listening: false, listenError: "Address already in use" }), false), /not listening/);
  assert.match(routerLine(manual({ listening: false }), false).title, /manual/i);
});

test("each failure says what it means", () => {
  const say = (reason, extra = {}) => routerLine(status({ state: "failed", reason, ...extra }), false);
  assert.match(say("noGateway").detail, /UPnP/);
  assert.match(say("refused").title, /refused/i);
  assert.match(say("portInUse").detail, /different port/);
  assert.match(say("permanentOnly").title, /for good/);
  assert.match(say("permanentOnly").detail, /by hand/);
  assert.doesNotMatch(say("permanentOnly").title, /unknown/i);
  assert.match(say("other", { detail: "ConflictInMappingEntry" }).detail, /ConflictInMappingEntry/);
  assert.doesNotMatch(say("other").detail, /undefined/);
});

test("the listener reports its own failure before anything else", () => {
  const broken = listenLine(status({ state: "off" }, { listening: false, listenError: "Address already in use" }));
  assert.equal(broken.tone, "bad");
  assert.match(broken.detail, /Address already in use/);
  assert.equal(listenLine(status({ state: "off" })).tone, "ok");

  // Quoted errors arrive with and without their own full stop.
  const dotted = listenLine(status({ state: "off" }, { listening: false, listenError: "Permission denied." }));
  assert.doesNotMatch(dotted.detail, /\.\./);
  const router = routerLine(status({ state: "failed", reason: "other", detail: "Conflict in mapping entry." }), false);
  assert.doesNotMatch(router.detail, /\.\./);
  assert.equal(listenLine(status({ state: "off" }, { listening: false })).tone, "info");
});

test("a missing link explains what is missing", () => {
  assert.match(linkPendingReason(status({ state: "searching" }), false), /still working out/);
  const byHand = linkPendingReason(manual(), false);
  assert.match(byHand, /did not tell Geode its public address/);
  assert.match(byHand, /Public address in Settings/);
  const failed = linkPendingReason(status({ state: "failed", reason: "noGateway" }), false);
  assert.match(failed, /did not open the port or report an address/);
  assert.match(failed, /forwarded by hand/);
  assert.match(failed, /Public address in Settings/);
  assert.match(linkPendingReason(null, false), /still working out/);
});

test("manual setup shows the address the router reported, and what a link will use", () => {
  const found = manual({ detected: { ip: "203.0.113.7", reachability: "public" } });
  const line = routerLine(found, false);
  assert.match(line.title, /manual/i);
  assert.notEqual(line.tone, "ok");
  assert.match(line.detail, /Forward TCP port 25590/);
  assert.match(line.detail, /203\.0\.113\.7:25590/);
  assert.doesNotMatch(line.detail, /Public address in Settings/);
  // The link comes from it, so there is nothing to explain but the wait.
  assert.match(linkPendingReason(found, false), /still working out/);

  // A public address set in Settings is what links use, not the router's.
  const overridden = routerLine(found, true);
  assert.doesNotMatch(overridden.detail, /links use 203/);
  assert.match(overridden.detail, /public address from Settings/);

  // Nothing learned: say so, and where the address goes instead.
  const unknown = routerLine(manual(), false);
  assert.match(unknown.detail, /did not say what its public address is/);
  assert.match(unknown.detail, /Public address in Settings/);
});

test("manual setup says nothing about the router's answer before it has one", () => {
  // Still asking the first time: not a reason to go and type an address.
  const asking = manual({ asking: true });
  assert.match(routerLine(asking, false).detail, /is asking your router/);
  assert.doesNotMatch(routerLine(asking, false).detail, /did not say|Public address in Settings/);
  assert.match(linkPendingReason(asking, false), /still working out/);

  // Not listening: the router has not been asked at all.
  const idle = manual({ listening: false });
  assert.doesNotMatch(routerLine(idle, false).detail, /did not say|Public address in Settings/);
  assert.match(routerLine(idle, false).detail, /once it is listening/);
  assert.match(linkPendingReason(idle, false), /still working out/);
});

test("a router that answered but would not open the port points at manual setup, which reads its address", () => {
  for (const [reason, extra] of [["refused", {}], ["permanentOnly", {}], ["other", { detail: "Conflict" }]]) {
    const failed = status({ state: "failed", reason, ...extra });
    assert.match(routerLine(failed, false).detail, /Set it up myself/, reason);
    assert.match(linkPendingReason(failed, false), /Set it up myself/, reason);
  }
  // No router answered, or the port is someone else's: manual setup would learn nothing more.
  for (const reason of ["noGateway", "portInUse"]) {
    const failed = status({ state: "failed", reason });
    assert.doesNotMatch(routerLine(failed, false).detail, /Set it up myself/, reason);
    assert.doesNotMatch(linkPendingReason(failed, false), /Set it up myself/, reason);
  }
});

test("manual setup behind carrier-grade NAT or a second router says so before anyone forwards anything", () => {
  const shared = manual({ detected: { ip: "100.64.1.2", reachability: "cgnat" } });
  const cgnat = routerLine(shared, false);
  assert.equal(cgnat.tone, "bad");
  assert.match(cgnat.title, /cannot reach/i);
  assert.match(cgnat.detail, /100\.64\.1\.2/);
  assert.doesNotMatch(cgnat.detail, /opened the port/);
  const cgnatReason = linkPendingReason(shared, false);
  assert.match(cgnatReason, /100\.64\.1\.2/);
  assert.doesNotMatch(cgnatReason, /your public IP/);

  const doubled = manual({ port: 31337, detected: { ip: "192.168.1.2", reachability: "doubleNat" } });
  const line = routerLine(doubled, false);
  assert.match(line.title, /another router/i);
  assert.match(line.detail, /31337 on both routers/);
  assert.match(linkPendingReason(doubled, false), /192\.168\.1\.2/);

  assert.equal(routerLine(manual({ detected: { ip: "0.0.0.0", reachability: "noIpv4" } }), false).tone, "bad");
});

test("an address read out off the default port says the port has to be typed", () => {
  // A player's Geode looks a bare host up on 25590, not on the admin's port.
  for (const address of ["127.0.0.1:38033", "host.playit.gg:12345", "[2001:db8::1]:40000"]) {
    const hint = readOutHint(address);
    assert.match(hint, /port included/, address);
    assert.match(hint, /25590/, address);
  }
  for (const address of ["203.0.113.7:25590", "play.example.com:25590", "[2001:db8::1]:25590"]) {
    const hint = readOutHint(address);
    assert.doesNotMatch(hint, /port included/, address);
    assert.match(hint, /leave the port off/, address);
  }

  // An https:// address has no port to add, but does need its scheme.
  const https = readOutHint("https://pack.example.com");
  assert.match(https, /https:\/\/ included/);
  assert.doesNotMatch(https, /port/);
  // And a link to one is checked no better than typing it, so do not say so.
  assert.doesNotMatch(https, /first connection/);

  // With no address known yet, nothing is claimed about one.
  const none = readOutHint(null);
  assert.match(none, /Join a server/);
  assert.doesNotMatch(none, /port/);
});

test("first use is the one verification that is not a plain yes", () => {
  for (const typed of [false, true]) {
    assert.equal(verificationLine("pinned", typed).tone, "ok");
    assert.equal(verificationLine("domain", typed).tone, "ok");
    const first = verificationLine("firstUse", typed);
    assert.equal(first.tone, "warn");
    assert.match(first.detail, /link from the server's admin/);
    assert.notEqual(verificationLine("pinned", typed).title, verificationLine("domain", typed).title);
  }
});

test("first use promises only what importing remembers", () => {
  // The fingerprint is remembered by the instance an import makes, not by
  // looking a server up, so "from here on" with no condition would be untrue.
  const first = verificationLine("firstUse", true);
  assert.doesNotMatch(first.title, /from now on/);
  assert.doesNotMatch(first.detail, /from here on/);
  assert.match(first.detail, /Importing remembers it/);
  assert.match(first.detail, /different certificate is refused/);
});

test("a typed address pinned by an earlier join is not credited to a link", () => {
  const remembered = verificationLine("pinned", true);
  assert.doesNotMatch(remembered.title, /link/);
  assert.doesNotMatch(remembered.detail, /in the link/);
  assert.match(remembered.detail, /joined this address before/);

  const linked = verificationLine("pinned", false);
  assert.match(linked.detail, /in the link/);
});

test("a stopped share promises only what sharing again brings back", () => {
  const off = stoppedLine(false);
  assert.equal(off.tone, "info");
  assert.equal(off.title, "Sharing is off");
  // A password replaced while stopped is not the one in links already sent, so
  // those are not promised to work again: only links carrying the one it has.
  assert.doesNotMatch(off.detail, /links you already sent/);
  assert.match(off.detail, /links you sent with it work again/);
  assert.doesNotMatch(off.detail, /config/);

  // A config folder that was being sent goes again, and the admin is told
  // before sharing again rather than after.
  assert.match(stoppedLine(true).detail, /also sends the config folder/);
});

test("a password replaced while sharing is stopped is not sent before sharing again", () => {
  assert.equal(replacedPasswordBody(true), "Send players the new link or password.");
  // The new link reaches nothing until the server is shared again.
  assert.match(replacedPasswordBody(false), /^Share the server again, then send players the new link or password\.$/);
});
