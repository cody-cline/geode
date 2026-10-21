// Run with `npm test` (Node 22.18 or newer, which strips the types from
// share.ts itself). Plain JavaScript on purpose: the project has no test
// runner and no Node typings, and TypeScript leaves .mjs files out of its
// check.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

import {
  OLD_ADDRESS_LABEL,
  addressChangedLine,
  arrivalLine,
  driveLine,
  existingLine,
  forbiddenAsTyped,
  forbiddenLabel,
  forbiddenPlayerHint,
  forbiddenRuleHint,
  gameAddressHint,
  joinInput,
  joinStep,
  linkKind,
  linkPendingReason,
  linksPointAtOldAddress,
  listenLine,
  localAddressHint,
  listingLine,
  nextJoin,
  nextPack,
  outcomeLine,
  packShareMessage,
  packStep,
  publicLinkLine,
  publishSummary,
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
    retry: null,
    mapping,
    address: null,
    localAddress: null,
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
  const promise = /can download this server's pack/i;
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

test("double NAT says how long a change out there takes to show up", () => {
  // The mapper reads the external address again at half the lease, and manual
  // detection asks again on the same half hour, so an admin who has bridged
  // the modem waits rather than going back through Settings.
  const opened = routerLine(status(mapped("doubleNat", { externalIp: "192.168.1.20" })), false);
  assert.match(opened.detail, /half an hour/);
  const byHand = routerLine(manual({ detected: { ip: "192.168.1.2", reachability: "doubleNat" } }), false);
  assert.match(byHand.detail, /half an hour/);
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

test("the open port is the one packs come down, not the one Minecraft uses", () => {
  // Geode maps the pack port and nothing else, so a line about connecting
  // would be read as the game address and handed out as one.
  const line = routerLine(status(mapped("public")), false);
  assert.doesNotMatch(line.detail, /connect at/);
  assert.match(line.detail, /download this server's pack/);
  assert.match(line.detail, /203\.0\.113\.7:25590/);
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

/** A listener that could not start and is being tried again. */
const retrying = (retry) =>
  status(
    { state: "off" },
    { listening: false, listenError: "Address already in use", retry },
  );

test("a listener still being tried again is not a listener that is broken", () => {
  const trying = listenLine(retrying({ attempts: 2, nextInSeconds: 8, needsYou: false }));
  assert.equal(trying.tone, "warn");
  // "Within", because the number stands still: it is worked out when the
  // status is sent, and the status is only sent once an attempt, so a tab
  // left open reads the same wait for the whole of it.
  assert.match(trying.detail, /trying again within 8 seconds/);
  assert.doesNotMatch(trying.detail, /trying again in 8 seconds/);
  // The attempt named is the one to come, not the ones behind it.
  assert.match(trying.detail, /attempt 3/);
  assert.match(trying.detail, /Address already in use/);

  // A wait of no seconds left reads as a moment rather than "in 0 seconds".
  const imminent = listenLine(retrying({ attempts: 1, nextInSeconds: 1, needsYou: false }));
  assert.match(imminent.detail, /in a moment/);
  assert.doesNotMatch(imminent.detail, /in 1 seconds/);

  // Past the threshold it is the admin's to fix, and says what to do.
  const stuck = listenLine(retrying({ attempts: 5, nextInSeconds: 60, needsYou: true }));
  assert.equal(stuck.tone, "bad");
  assert.match(stuck.detail, /5 attempts/);
  assert.match(stuck.detail, /close the program/);
  assert.match(stuck.detail, /Address already in use/);

  // Changing the port is never the advice that comes first, and never without
  // saying what it costs: every link already sent carries the old one.
  for (const line of [trying, stuck]) {
    assert.match(line.detail, /breaks every link you have already sent/);
    const port = line.detail.indexOf("Changing the port");
    assert.ok(port > line.detail.length / 2, `port advice leads: ${line.detail}`);
  }
  // And it is still said for a failure nobody is retrying.
  assert.match(
    listenLine(status({ state: "off" }, { listening: false, listenError: "Permission denied" })).detail,
    /breaks every link you have already sent/,
  );

  // The missing link follows: late while it tries, and not a dead end.
  assert.match(
    linkPendingReason(retrying({ attempts: 2, nextInSeconds: 8, needsYou: false }), false),
    /trying again/,
  );
  assert.doesNotMatch(
    linkPendingReason(retrying({ attempts: 5, nextInSeconds: 60, needsYou: true }), false),
    /trying again/,
  );
});

test("the router note does not tell the admin to fix what Geode is fixing", () => {
  // The listening note printed directly above says to leave the port alone
  // while Geode tries for it; "fix that first" here answers the same second
  // the other way.
  const trying = retrying({ attempts: 2, nextInSeconds: 8, needsYou: false });
  assert.doesNotMatch(routerLine(trying, false).detail, /fix that first/);
  assert.match(routerLine(trying, false).detail, /as soon as it is listening/);

  // Once nobody expects trying to work, it is the admin's again.
  const stuck = retrying({ attempts: 5, nextInSeconds: 60, needsYou: true });
  assert.match(routerLine(stuck, false).detail, /fix that first/);
  const alone = status({ state: "off" }, { listening: false, listenError: "Address already in use" });
  assert.match(routerLine(alone, false).detail, /fix that first/);
});

test("the lines about a working port say a firewall can still stop it", () => {
  // The listener binds every interface, which is what raises Windows' prompt;
  // denying it leaves the router showing the port open and every player
  // timing out, with nothing else on the page pointing at it.
  const listening = listenLine(status({ state: "off" }));
  assert.equal(listening.tone, "ok");
  assert.match(listening.detail, /firewall/);
  assert.match(listening.detail, /Windows/);

  // Once on the panel: the router line is printed directly beneath this one,
  // and it is the listener Windows asks about.
  assert.doesNotMatch(routerLine(manual(), false).detail, /firewall/);
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

test("every router failure says Geode keeps asking on its own", () => {
  // The mapper retries ten seconds on, at most a minute apart after that, and
  // the page follows. Without saying so, an admin who has just switched UPnP
  // on restarts Geode or changes the port — the one change that breaks links
  // already handed out.
  for (const reason of ["noGateway", "refused", "portInUse", "permanentOnly", "other"]) {
    const line = routerLine(status({ state: "failed", reason, detail: "Conflict" }), false);
    assert.match(line.detail, /keeps asking/, reason);
    assert.match(line.detail, /within a minute/, reason);
  }
});

test("a public address already set is not asked for by a router that refused", () => {
  // Links use the Settings address whatever the router says, so switching to
  // manual detection could not change theirs: it would only ask them to expose
  // a port for nothing.
  for (const [reason, extra] of [["refused", {}], ["permanentOnly", {}], ["other", { detail: "Conflict" }]]) {
    const failed = status({ state: "failed", reason, ...extra });
    assert.match(routerLine(failed, false).detail, /Set it up myself/, reason);
    assert.doesNotMatch(routerLine(failed, true).detail, /Set it up myself/, reason);
    assert.doesNotMatch(routerLine(failed, true).detail, /reads your public address/, reason);
    // Still the failure and still the port to forward, only without the aside.
    assert.match(routerLine(failed, true).detail, /by hand|yourself/, reason);
  }
});

test("a router that answered but would not open the port points at manual setup, which reads its address", () => {
  for (const [reason, extra] of [["refused", {}], ["permanentOnly", {}], ["other", { detail: "Conflict" }]]) {
    const failed = status({ state: "failed", reason, ...extra });
    const detail = routerLine(failed, false).detail;
    assert.match(detail, /Set it up myself/, reason);
    assert.match(linkPendingReason(failed, false), /Set it up myself/, reason);
    // The retry comes before that aside, never after it: manual setup stops
    // the mapper, so a minute read as what follows switching to it would be
    // wrong — detection asks the router again every half hour.
    assert.ok(detail.indexOf("keeps asking") < detail.indexOf("Set it up myself"), reason);
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

/** A shared server whose address players are given, as the read-out sees it. */
const readOutAt = (address) => readOutHint(status(mapped("public"), { address }));

test("an address read out off the default port says the port has to be typed", () => {
  // A player's Geode looks a bare host up on 25590, not on the admin's port.
  for (const address of ["127.0.0.1:38033", "host.playit.gg:12345", "[2001:db8::1]:40000"]) {
    const hint = readOutAt(address);
    assert.match(hint, /port included/, address);
    assert.match(hint, /25590/, address);
  }
  for (const address of ["203.0.113.7:25590", "play.example.com:25590", "[2001:db8::1]:25590"]) {
    const hint = readOutAt(address);
    assert.doesNotMatch(hint, /port included/, address);
    assert.match(hint, /leave the port off/, address);
  }

  // An https:// address has no port to add, but does need its scheme.
  const https = readOutAt("https://pack.example.com");
  assert.match(https, /https:\/\/ included/);
  assert.doesNotMatch(https, /port/);
  // And a link to one is checked no better than typing it, so do not say so.
  assert.doesNotMatch(https, /first connection/);

  // With no address known yet, nothing is claimed about one.
  const none = readOutHint(null);
  assert.match(none, /Join a server/);
  assert.doesNotMatch(none, /port/);
});

test("an address that is never coming is not read out as one on its way", () => {
  // Behind carrier-grade NAT, a second router or with no IPv4 the address
  // stays null for good, so "once Geode knows it" would be a promise nothing
  // can keep. What a player does with the password is still said here, where
  // nothing else says it; where to set an address that works is not, because
  // the notes above and below this block already do.
  for (const reachability of ["cgnat", "doubleNat", "noIpv4"]) {
    const hint = readOutHint(status(mapped(reachability)));
    assert.doesNotMatch(hint, /once Geode knows it/, reachability);
    assert.match(hint, /Join a server/, reachability);
    assert.doesNotMatch(hint, /Public address in Settings/, reachability);
  }
  const detected = readOutHint(manual({ detected: { ip: "100.64.1.2", reachability: "cgnat" } }));
  assert.doesNotMatch(detected, /once Geode knows it/);
  assert.match(detected, /Join a server/);

  // An address still being worked out is still worth waiting for.
  for (const pending of [status({ state: "searching" }), status(mapped("public")), manual()]) {
    assert.match(readOutHint(pending), /once Geode knows it/);
  }
});

test("the address for this network says a link tried from here can fail anyway", () => {
  const here = localAddressHint(
    status(mapped("public"), { address: "203.0.113.7:25590", localAddress: "192.168.1.50:25590" }),
  );
  assert.match(here, /same house or on the same Wi-Fi/);
  // The failure the admin meets first: their own link, from their own machine.
  assert.match(here, /this computer included/);
  assert.match(here, /out to your router and come back in/);
  // And it is the router, not the share, so nobody goes looking at the port.
  assert.match(here, /players elsewhere are unaffected/);

  // Through a domain or tunnel the trip is not the router's to refuse, so the
  // reason names what is actually in the way.
  const tunnelled = localAddressHint(
    status(mapped("public"), { address: "https://pack.example.com", localAddress: "192.168.1.50:25590" }),
  );
  assert.match(tunnelled, /domain or tunnel/);
  assert.doesNotMatch(tunnelled, /your router/);

  // No link to try yet: the address is still what players here use, and
  // nothing is said about trying a link that does not exist.
  for (const pending of [null, status(mapped("cgnat"), { localAddress: "192.168.1.50:25590" })]) {
    const hint = localAddressHint(pending);
    assert.match(hint, /same house or on the same Wi-Fi/);
    assert.doesNotMatch(hint, /can fail/);
  }
});

test("links already sent are called stale only once the address has really moved", () => {
  const at = (address, extra = {}) => status(mapped("public"), { address, ...extra });

  const moved = addressChangedLine(at("198.51.100.4:25590"), "203.0.113.7:25590");
  assert.equal(moved.tone, "warn");
  assert.match(moved.detail, /203\.0\.113\.7:25590/);
  assert.match(moved.detail, /no longer work/);
  // The link above is the answer; the instances players already made are not
  // this note's to promise anything about.
  assert.match(moved.detail, /link above/);
  assert.doesNotMatch(moved.detail, /instance/);

  // The same address, so nothing has gone stale — and taking a link at the
  // address of the day is what puts it here.
  assert.equal(addressChangedLine(at("203.0.113.7:25590"), "203.0.113.7:25590"), null);
  // No link ever taken: nobody has been sent one to go stale.
  assert.equal(addressChangedLine(at("198.51.100.4:25590"), null), null);
  // And with no address known there is no new link to send instead, so the
  // note would be a complaint with no answer to it.
  assert.equal(addressChangedLine(at(null), "203.0.113.7:25590"), null);
  assert.equal(addressChangedLine(null, "203.0.113.7:25590"), null);
});

test("the badge on a server list and the sentence under its link agree", () => {
  const at = (address) => status(mapped("public"), { address });

  // The one case there is a badge for: a link was taken, and the address it
  // was taken at is not the one this computer has now.
  assert.equal(
    linksPointAtOldAddress(at("198.51.100.4:25590"), "203.0.113.7:25590"),
    true,
  );
  // Nothing has moved.
  assert.equal(
    linksPointAtOldAddress(at("203.0.113.7:25590"), "203.0.113.7:25590"),
    false,
  );
  // No link taken — and a server that is not shared reaches this the same way,
  // because the backend names an address only while it is shared.
  assert.equal(linksPointAtOldAddress(at("198.51.100.4:25590"), null), false);
  // No address known, so there is no new link to badge the old one against.
  assert.equal(linksPointAtOldAddress(at(null), "203.0.113.7:25590"), false);
  assert.equal(linksPointAtOldAddress(null, "203.0.113.7:25590"), false);

  // One comparison, so a badge and a sentence can never disagree.
  for (const [address, taken] of [
    ["198.51.100.4:25590", "203.0.113.7:25590"],
    ["203.0.113.7:25590", "203.0.113.7:25590"],
    ["198.51.100.4:25590", null],
    [null, "203.0.113.7:25590"],
  ]) {
    assert.equal(
      linksPointAtOldAddress(at(address), taken),
      addressChangedLine(at(address), taken) !== null,
    );
  }

  // The label the badge carries for a screen reader says which way round it is:
  // the links are old, not the address the Share tab now shows.
  assert.match(OLD_ADDRESS_LABEL, /links you sent/);
  assert.match(OLD_ADDRESS_LABEL, /old address/);
});

// The badge's two halves have different lifetimes: the address Geode can see
// now arrives as an event, while `lastLinkAddress` only ever reaches the
// sidebar and the Servers cards inside the `list_servers` payload the store
// caches. Nothing polls and no event reloads that list, so a Share tab handler
// that changes what the payload would say has to ask for it again — or the
// badge misses the change it exists for and then keeps claiming it after the
// admin has done what it asked. There is no DOM or React renderer under
// `npm test`, so the invariant is checked where it is written.
test("every share change the lists badge also refreshes the server list", async () => {
  const source = await readFile(
    new URL("../views/server/ShareTab.tsx", import.meta.url),
    "utf8",
  );

  // The three calls that change what `list_servers` answers for
  // `lastLinkAddress`: taking a link writes the address, and sharing or
  // stopping decides whether it is reported at all.
  const changesTheList = [
    "api.shareServer(",
    "api.unshareServer(",
    "api.shareLinkCopied(",
  ];

  // Every handler is `async function` at the component's own indentation, and
  // closes with the first `}` back at it.
  const handlers = source.split("\n  async function ").slice(1);
  const covered = [];
  for (const handler of handlers) {
    const name = handler.slice(0, handler.indexOf("("));
    const body = handler.slice(0, handler.indexOf("\n  }"));
    for (const call of changesTheList) {
      if (!body.includes(call)) continue;
      covered.push(call);
      assert.match(
        body,
        /refreshServers\(\)/,
        `${name}() calls ${call}) without refreshing the server list, so the stale-address badge cannot follow it`,
      );
    }
  }
  assert.deepEqual(
    covered.sort(),
    [...changesTheList].sort(),
    "a call that changes what the server list says moved out of a handler, or was renamed",
  );
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
  // The pin is derived from instances and nothing else, so the memory ends
  // with the last instance joined from that address.
  assert.match(first.detail, /as long as an instance joined from it is still there/);
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

test("the admin is told plainly that a mod rule is a rule and not a wall", () => {
  const hint = forbiddenRuleHint();
  // What it does: off, never deleted, and again on every update.
  assert.match(hint, /turned off, never deleted/);
  assert.match(hint, /every time this server's pack updates/);
  // What it is not. This is the half an admin would be hurt by not reading, so
  // it must name the three ways round it and whose job real enforcement is.
  assert.match(hint, /no anti-cheat/);
  assert.match(hint, /patched Geode, another launcher, or a jar added after an update/);
  assert.match(hint, /your server software's job/);
  // And no word that would read as a guarantee.
  assert.doesNotMatch(hint, /\b(prevents|blocks|stops players|cannot run|enforced)\b/);
});

test("the player is told their mod is off and still theirs, and that turning it back on does not last", () => {
  const hint = forbiddenPlayerHint();
  assert.match(hint, /never deletes them/);
  assert.match(hint, /stays in your mods folder/);
  assert.match(hint, /the server's reason/);
  // The one thing they would otherwise learn by trying it.
  assert.match(hint, /does not last/);
});

test("a rule naming only a mod id carries no site, which is the only pair the backend takes", () => {
  // What the fields hold when the admin typed the mod id the tab's own
  // placeholder suggests and left the project id blank. The site select is
  // never empty, so the site has to come off here — `Forbidden::checked`
  // refuses a site with no project id beside it, which would make the mod id
  // field, the only way to name a jar somebody dropped in by hand, unusable.
  assert.deepEqual(
    forbiddenAsTyped({
      provider: "modrinth",
      projectId: "",
      modId: "xaerominimap",
      reason: "no x-ray on this server",
    }),
    { provider: "", projectId: "", modId: "xaerominimap", reason: "no x-ray on this server" },
  );
  // A project id keeps the site it was typed under: it means nothing without
  // one, and that pair is refused too.
  assert.deepEqual(
    forbiddenAsTyped({ provider: "curseforge", projectId: "238222", modId: "", reason: "no jei" }),
    { provider: "curseforge", projectId: "238222", modId: "", reason: "no jei" },
  );
  // And whitespace is not a project id for a site to belong to.
  assert.equal(
    forbiddenAsTyped({ provider: "modrinth", projectId: "   ", modId: "jei", reason: "no" })
      .provider,
    "",
  );
});

test("a rule says which kind of name it was given, because the two match differently", () => {
  assert.equal(
    forbiddenLabel({ provider: "", projectId: "", modId: "xaerominimap", reason: "no" }),
    "mod id xaerominimap",
  );
  assert.equal(
    forbiddenLabel({ provider: "modrinth", projectId: "AANobbMI", modId: "", reason: "no" }),
    "Modrinth project AANobbMI",
  );
  assert.equal(
    forbiddenLabel({ provider: "curseforge", projectId: "238222", modId: "jei", reason: "no" }),
    "mod id jei, CurseForge project 238222",
  );
});

test("a password replaced while sharing is stopped is not sent before sharing again", () => {
  assert.equal(replacedPasswordBody(true), "Send players the new link or password.");
  // The new link reaches nothing until the server is shared again.
  assert.match(replacedPasswordBody(false), /^Share the server again, then send players the new link or password\.$/);
});

// ---------------------------------------------------------------------------
// Sharing a pack through Google Drive
// ---------------------------------------------------------------------------

function report(over = {}) {
  return {
    referenced: 0,
    embeddedUnknown: 0,
    embeddedElsewhere: 0,
    embeddedTurnedOff: 0,
    unnameable: [],
    trouble: [],
    ...over,
  };
}

function driveStatus(over = {}) {
  return {
    configured: true,
    connected: true,
    needsReconnect: false,
    account: "someone@example.com",
    folderName: "Geode Packs",
    folderUrl: "https://drive.google.com/drive/folders/folder-1",
    ...over,
  };
}

test("linkKind tells the two kinds of link apart and refuses anything else", () => {
  assert.equal(linkKind("geode://join?addr=203.0.113.7:25590&code=7K2QF9XWMP"), "join");
  assert.equal(linkKind("geode://pack?drive=abc&h=" + "ab".repeat(32)), "pack");
  // Case and one trailing slash are the only forgiveness, the same as the
  // parser in Rust.
  assert.equal(linkKind("GEODE://PACK?drive=abc"), "pack");
  assert.equal(linkKind("geode://pack/?drive=abc"), "pack");
  assert.equal(linkKind("  geode://pack?drive=abc  "), "pack");
  for (const junk of [
    "geode://packs?drive=abc",
    "geode://",
    "https://pack.example.com",
    "",
    "geode://@pack?drive=abc",
  ]) {
    assert.equal(linkKind(junk), null, junk);
  }
});

test("the same pack link twice opens one dialog", () => {
  const first = nextPack(null, "geode://pack?drive=a&h=1", 1);
  assert.equal(nextPack(first, "geode://pack?drive=a&h=1", 2), first);
  const second = nextPack(first, "geode://pack?drive=b&h=2", 2);
  assert.notEqual(second, first);
  assert.equal(second.seq, 2);
});

test("packStep walks look, download, import, done and offers import once", () => {
  const view = { source: "drive", host: "drive.google.com", hash: "ab".repeat(32) };
  const arrival = { stage: "s", preview: { name: "Survival" } };
  const instance = { id: "i", name: "Survival" };
  assert.equal(packStep(null, null, null), "look");
  assert.equal(packStep(view, null, null), "download");
  assert.equal(packStep(view, arrival, null), "import");
  assert.equal(packStep(view, arrival, instance), "done");
  // Once an instance exists there is nothing to offer, whatever else is in
  // hand: importing again would make a second one.
  assert.equal(packStep(null, null, instance), "done");
});

test("the arrival line names the source parsing proved, not one the link claimed", () => {
  assert.match(
    arrivalLine({ source: "drive", host: "drive.google.com", hash: "" }).title,
    /Google Drive/,
  );
  assert.match(
    arrivalLine({ source: "url", host: "files.example.com", hash: "" }).title,
    /files\.example\.com/,
  );
});

test("the share message puts the Geode link first and ends on who can download it", () => {
  const message = packShareMessage({
    name: "Survival with Create",
    link: "geode://pack?drive=1A2b3C&h=" + "ab".repeat(32),
    fileUrl: "https://drive.google.com/file/d/1A2b3C/view",
    size: "18.4 MB",
  });
  const lines = message.split("\n");
  assert.match(lines[0], /^Survival with Create — a Minecraft pack for Geode$/);
  const geodeAt = lines.findIndex((line) => line.startsWith("geode://pack"));
  const driveAt = lines.findIndex((line) => line.startsWith("https://drive.google.com"));
  assert.ok(geodeAt > 0, "the message carries the geode:// link");
  assert.ok(driveAt > geodeAt, "the plain link should follow the geode:// one");
  assert.match(message, /\(18\.4 MB\)/);
  assert.equal(lines[lines.length - 1], "Anyone with these links can download it.");
  assert.match(message, /\.mrpack any launcher can open/);
});

test("publishSummary says references and published bytes apart", () => {
  const [references, bytes] = publishSummary(
    report({ referenced: 31, embeddedUnknown: 2 }),
  );
  assert.match(references, /31 files travel as a reference/);
  assert.match(bytes, /2 files travel as bytes/);
  assert.match(bytes, /no provider could name them/);
  assert.match(bytes, /Uploading publishes those copies/);

  // One of each reads as one of each.
  const single = publishSummary(report({ referenced: 1, embeddedTurnedOff: 1 }));
  assert.match(single[0], /1 file travels as a reference/);
  assert.match(single[1], /because it is turned off/);

  // Nothing to say is said rather than left blank.
  assert.deepEqual(publishSummary(report()), ["This pack carries no mods at all."]);
});

test("publishSummary names the files that could not go in while there are few enough", () => {
  const few = publishSummary(report({ unnameable: ["mods/odd.jar", "mods/older.jar"] }));
  assert.match(few[0], /mods\/odd\.jar/);
  assert.match(few[0], /mods\/older\.jar/);

  const many = publishSummary(report({ unnameable: Array.from({ length: 9 }, (_, i) => `m${i}.jar`) }));
  assert.match(many[0], /^9 files could not go in at all\.$/);

  // Trouble the export ran into is carried through, ended properly.
  const [trouble] = publishSummary(report({ trouble: ["CurseForge was not asked"] }));
  assert.equal(trouble, "CurseForge was not asked.");
});

test("the public-link check reads as a confirmation or as a warning", () => {
  assert.equal(publicLinkLine("served").tone, "ok");
  assert.match(publicLinkLine("served").title, /serves the pack/);
  assert.equal(publicLinkLine("webPage").tone, "warn");
  assert.match(publicLinkLine("webPage").detail, /Import a pack/);
  assert.equal(publicLinkLine("unknown").tone, "info");
});

test("the Drive line tells not configured, not connected and reconnect apart", () => {
  assert.match(driveLine(driveStatus({ configured: false })).title, /Not configured/);
  assert.match(
    driveLine(driveStatus({ configured: false })).detail,
    /export a \.geodepack and hand it over/,
  );
  assert.match(driveLine(driveStatus({ connected: false })).title, /Not connected/);
  const stale = driveLine(driveStatus({ connected: false, needsReconnect: true }));
  assert.match(stale.title, /Reconnect/);
  // The seven-day trap is named where it will be read.
  assert.match(stale.detail, /seven days/);
  assert.match(stale.detail, /Publish app/);
  const connected = driveLine(driveStatus());
  assert.match(connected.title, /Connected as someone@example\.com/);
  assert.match(connected.detail, /Geode Packs/);
  assert.match(connected.detail, /cannot see anything else/);
  assert.match(driveLine(driveStatus({ account: null })).title, /^Connected$/);
});

function described(listing, mcVersion = "1.21.1") {
  return {
    name: "Survival",
    mcVersion,
    loader: "fabric",
    loaderVersion: "0.16.9",
    mods: 4,
    serverMods: 0,
    serverKind: "fabric",
    revision: "r1",
    serverId: "survival",
    listing,
  };
}

function shareView(extra = {}) {
  return {
    code: "GEODE-ABCDE-FGHJK",
    link: null,
    includeConfig: false,
    shared: true,
    unsupported: null,
    listing: null,
    suggested: { name: "Survival", address: null },
    packAddress: null,
    ...extra,
  };
}

test("a player is told what importing adds to their server list, and only when there is one", () => {
  const line = listingLine(described({ name: "Survival Club", address: "play.example.com:25566" }));
  assert.equal(line.tone, "info");
  assert.match(line.title, /Survival Club/);
  assert.match(line.detail, /play\.example\.com:25566/);
  // Their own servers are not touched, and the game rewrites the list when it
  // closes — both things a player needs before pressing Import, not after.
  assert.match(line.detail, /own servers are left alone/);
  assert.match(line.detail, /Close Minecraft first/);

  // Play opens straight into the server only where the version's own manifest
  // declares quick play, which 1.19.4 and older never did — so an older pack
  // is told where the row is instead of being promised a jump.
  assert.match(line.detail, /points Play straight at it/);
  const old = listingLine(
    described({ name: "Survival Club", address: "play.example.com:25566" }, "1.19.4"),
  );
  assert.doesNotMatch(old.detail, /points Play straight at it/);
  assert.match(old.detail, /1\.19\.4/);
  assert.match(old.detail, /Multiplayer screen/);
  // A snapshot is not a release number this can read, so it is promised
  // nothing either.
  const snapshot = listingLine(described({ name: "Survival Club", address: "s" }, "23w31a"));
  assert.doesNotMatch(snapshot.detail, /points Play straight at it/);

  // A server on a Geode too old to send one, or an admin who set none, says
  // nothing at all rather than promising something that will not happen.
  assert.equal(listingLine(described(null)), null);
  assert.equal(listingLine(described(undefined)), null);
});

test("the admin is told which address is the pack's and which one players play on", () => {
  // Saved: both named, and what happens when it changes.
  const saved = gameAddressHint(
    shareView({
      packAddress: "203.0.113.7:25590",
      listing: { name: "Survival Club", address: "play.example.com:25566" },
    }),
  );
  assert.match(saved, /203\.0\.113\.7:25590/);
  assert.match(saved, /Geode's port/);
  assert.match(saved, /play\.example\.com:25566/);
  assert.match(saved, /nothing downloaded/);

  // Suggested but not saved: the admin is asked to check it, because a
  // forwarded port is something only they know.
  const suggested = gameAddressHint(
    shareView({
      packAddress: "203.0.113.7:25590",
      suggested: { name: "Survival", address: "203.0.113.7:25566" },
    }),
  );
  assert.match(suggested, /check it is the one you forwarded/);

  // Nothing to suggest — an https tunnel, or no public address at all — so
  // the admin is asked rather than given a guess.
  const asked = gameAddressHint(shareView({ packAddress: "https://pack.example.com" }));
  assert.match(asked, /cannot work out the game address/);
  assert.match(asked, /Add Server/);
  // And with no pack address either, the sentence still stands on its own.
  const nothing = gameAddressHint(shareView());
  assert.match(nothing, /Geode's own port/);
  assert.match(nothing, /cannot work out the game address/);
});
