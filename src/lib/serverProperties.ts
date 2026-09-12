/**
 * What the keys in `server.properties` mean.
 *
 * Presentation only. The file on disk stays the source of truth for which keys
 * exist — this table decorates the ones it recognises and says nothing about
 * the rest, so a key added by a future Minecraft version, a plugin or a fork
 * still appears and still saves. That is deliberate: a hard-coded list of
 * "supported" keys would start wrong the next time Mojang adds one.
 *
 * Nothing here adds a key that is not already in the file. Vanilla rewrites
 * the file on shutdown and drops what it does not recognise, so a key invented
 * by this UI would silently vanish.
 */

export type PropertyGroup =
  | "World"
  | "Players"
  | "Network"
  | "Performance"
  | "Security";

/** The order groups are shown in — most-reached-for first. */
export const GROUP_ORDER: PropertyGroup[] = [
  "World",
  "Players",
  "Network",
  "Performance",
  "Security",
];

export type Control =
  | { kind: "bool" }
  | { kind: "int"; min?: number; max?: number; unit?: string }
  | { kind: "enum"; options: { value: string; label: string }[] }
  | { kind: "text"; placeholder?: string; secret?: boolean };

export interface PropertyInfo {
  group: PropertyGroup;
  label: string;
  description: string;
  control: Control;
  /**
   * Why editing this while the server runs is a bad idea. Shown as a warning
   * rather than a block — it is the user's server.
   */
  danger?: string;
  /**
   * A console command that applies the same change immediately. The file makes
   * it stick across restarts; the command makes it true now.
   */
  liveCommand?: (value: string) => string;
}

const bool = (): Control => ({ kind: "bool" });
const int = (min?: number, max?: number, unit?: string): Control => ({
  kind: "int",
  min,
  max,
  unit,
});
const text = (placeholder?: string, secret?: boolean): Control => ({
  kind: "text",
  placeholder,
  secret,
});
const enumOf = (...values: [string, string][]): Control => ({
  kind: "enum",
  options: values.map(([value, label]) => ({ value, label })),
});

export const PROPERTY_INFO: Record<string, PropertyInfo> = {
  // --- World ---------------------------------------------------------------
  "level-name": {
    group: "World",
    label: "World folder",
    description:
      "The folder the world is saved in. Geode also keeps this server's datapacks inside it.",
    control: text("world"),
    danger:
      "The running server keeps saving to the old folder, and datapacks would be written somewhere it is not reading. This is the most common way to appear to lose a world.",
  },
  "level-seed": {
    group: "World",
    label: "World seed",
    description:
      "Seed for generating a new world. Ignored once the world exists.",
    control: text("blank for random"),
  },
  "level-type": {
    group: "World",
    label: "World type",
    description: "Terrain generator used when the world is first created.",
    control: enumOf(
      ["minecraft:normal", "Normal"],
      ["minecraft:flat", "Superflat"],
      ["minecraft:large_biomes", "Large biomes"],
      ["minecraft:amplified", "Amplified"],
      ["minecraft:single_biome_surface", "Single biome"],
    ),
  },
  "generator-settings": {
    group: "World",
    label: "Generator settings",
    description:
      "JSON describing custom terrain, mainly the layers of a superflat world.",
    control: text("{}"),
  },
  "generate-structures": {
    group: "World",
    label: "Generate structures",
    description:
      "Whether villages, temples and strongholds appear in new chunks.",
    control: bool(),
  },
  difficulty: {
    group: "World",
    label: "Difficulty",
    description:
      "How hard mobs hit, and whether hostile ones spawn at all.",
    control: enumOf(
      ["peaceful", "Peaceful"],
      ["easy", "Easy"],
      ["normal", "Normal"],
      ["hard", "Hard"],
    ),
    liveCommand: (value) => `difficulty ${value}`,
  },
  gamemode: {
    group: "World",
    label: "Default game mode",
    description: "The mode players start in when they first join.",
    control: enumOf(
      ["survival", "Survival"],
      ["creative", "Creative"],
      ["adventure", "Adventure"],
      ["spectator", "Spectator"],
    ),
    liveCommand: (value) => `defaultgamemode ${value}`,
  },
  "force-gamemode": {
    group: "World",
    label: "Force game mode",
    description:
      "Put players back into the default mode every time they join.",
    control: bool(),
  },
  hardcore: {
    group: "World",
    label: "Hardcore",
    description: "Death is permanent — players become spectators.",
    control: bool(),
    danger:
      "Turning this on changes what death means for everyone already playing on this world.",
  },
  "allow-nether": {
    group: "World",
    label: "Allow the Nether",
    description: "Whether nether portals work.",
    control: bool(),
  },
  pvp: {
    group: "World",
    label: "Player vs player",
    description: "Whether players can damage each other directly.",
    control: bool(),
  },
  "spawn-monsters": {
    group: "World",
    label: "Spawn monsters",
    description:
      "Hostile mob spawning. Redundant when the difficulty is peaceful.",
    control: bool(),
  },
  "spawn-animals": {
    group: "World",
    label: "Spawn animals",
    description:
      "Passive mob spawning. Does not remove animals already in the world.",
    control: bool(),
  },
  "spawn-npcs": {
    group: "World",
    label: "Spawn villagers",
    description: "Whether villagers spawn in generated villages.",
    control: bool(),
  },
  "max-world-size": {
    group: "World",
    label: "World border",
    description:
      "Radius in blocks past which the world stops generating.",
    control: int(1, 29999984, "blocks"),
    danger:
      "Shrinking the border strands any players and structures that are already outside it.",
  },

  // --- Players -------------------------------------------------------------
  "max-players": {
    group: "Players",
    label: "Player slots",
    description: "How many people can be connected at once.",
    control: int(0, 200),
  },
  motd: {
    group: "Players",
    label: "Server description",
    description:
      "The line shown under the server name in the multiplayer list.",
    control: text("A Minecraft Server"),
  },
  "allow-flight": {
    group: "Players",
    label: "Allow flight",
    description:
      "Permit flight from mods and elytra without the anti-cheat kicking players.",
    control: bool(),
  },
  "player-idle-timeout": {
    group: "Players",
    label: "Idle kick",
    description: "Kick a player after this long with no input. 0 never kicks.",
    control: int(0, undefined, "minutes"),
  },
  "white-list": {
    group: "Players",
    label: "Whitelist",
    description: "Only players on the whitelist may join.",
    control: bool(),
    liveCommand: (value) => `whitelist ${value === "true" ? "on" : "off"}`,
  },
  "enforce-whitelist": {
    group: "Players",
    label: "Enforce whitelist immediately",
    description:
      "Kick already-connected players who are not on the whitelist when it is reloaded.",
    control: bool(),
  },
  "hide-online-players": {
    group: "Players",
    label: "Hide the player list",
    description: "Leave player names out of the server-list ping.",
    control: bool(),
  },
  "resource-pack": {
    group: "Players",
    label: "Resource pack URL",
    description:
      "Pack offered to players when they join. Must be a direct download link.",
    control: text("https://…"),
  },
  "resource-pack-sha1": {
    group: "Players",
    label: "Resource pack checksum",
    description:
      "SHA-1 of the pack so clients can cache it. Strongly recommended whenever a URL is set.",
    control: text("40 hex characters"),
  },
  "resource-pack-prompt": {
    group: "Players",
    label: "Resource pack message",
    description: "Custom message on the accept or decline dialog.",
    control: text(),
  },
  "require-resource-pack": {
    group: "Players",
    label: "Require the resource pack",
    description: "Disconnect players who decline it.",
    control: bool(),
  },
  "broadcast-console-to-ops": {
    group: "Players",
    label: "Show console commands to ops",
    description: "Ops see commands run from the server console.",
    control: bool(),
  },
  "broadcast-rcon-to-ops": {
    group: "Players",
    label: "Show RCON commands to ops",
    description: "Ops see commands run over RCON.",
    control: bool(),
  },

  // --- Network -------------------------------------------------------------
  "server-ip": {
    group: "Network",
    label: "Bind address",
    description:
      "Which local network interface to listen on. Almost always left blank.",
    control: text("every interface"),
    danger: "Changes the address players connect to.",
  },
  "server-port": {
    group: "Network",
    label: "Port",
    description: "The port players connect to.",
    control: int(1, 65535),
    danger: "Changes the address players connect to.",
  },
  "online-mode": {
    group: "Network",
    label: "Verify accounts with Mojang",
    description:
      "Off means anyone can join under any username. Only correct behind a proxy on a closed network.",
    control: bool(),
    danger:
      "Turning this off lets anyone join as anyone, including as an operator. It is the most consequential switch in this file.",
  },
  "enable-status": {
    group: "Network",
    label: "Answer server-list pings",
    description:
      "Off makes the server look offline in the multiplayer list while still accepting joins.",
    control: bool(),
  },
  "prevent-proxy-connections": {
    group: "Network",
    label: "Block proxied connections",
    description:
      "Reject players whose Mojang-reported region does not match their address.",
    control: bool(),
    danger: "Can lock out an existing proxy setup.",
  },
  "network-compression-threshold": {
    group: "Network",
    label: "Compression threshold",
    description:
      "Packets larger than this are compressed. -1 disables compression, 0 compresses everything.",
    control: int(-1, undefined, "bytes"),
    danger:
      "Disabling compression over the internet makes the server close to unusable.",
  },
  "rate-limit": {
    group: "Network",
    label: "Packet rate limit",
    description: "Kick players who exceed this many packets a second. 0 is unlimited.",
    control: int(0),
    danger: "A low value kicks legitimate players.",
  },
  "enable-query": {
    group: "Network",
    label: "Enable the query protocol",
    description: "Expose a status endpoint for external monitoring tools.",
    control: bool(),
  },
  "query.port": {
    group: "Network",
    label: "Query port",
    description: "Port that status endpoint listens on.",
    control: int(1, 65535),
  },
  "enable-rcon": {
    group: "Network",
    label: "Enable RCON",
    description: "Allow remote console access over the network.",
    control: bool(),
    danger:
      "RCON refuses to start with a blank password, and the password is stored here in plain text.",
  },
  "rcon.port": {
    group: "Network",
    label: "RCON port",
    description: "Port RCON listens on.",
    control: int(1, 65535),
  },
  "rcon.password": {
    group: "Network",
    label: "RCON password",
    description:
      "Password for remote console. Stored in plain text in this file, beside the world.",
    control: text(undefined, true),
    danger: "Stored in plain text, and RCON will not start without one.",
  },
  "use-native-transport": {
    group: "Network",
    label: "Use native networking",
    description: "A Linux-only optimisation. Harmless elsewhere.",
    control: bool(),
  },

  // --- Performance ---------------------------------------------------------
  "view-distance": {
    group: "Performance",
    label: "View distance",
    description:
      "How far players can see. The single biggest lever on processor load and bandwidth.",
    control: int(2, 32, "chunks"),
  },
  "simulation-distance": {
    group: "Performance",
    label: "Simulation distance",
    description:
      "How far mobs, crops and redstone keep ticking. Should not exceed the view distance.",
    control: int(3, 32, "chunks"),
  },
  "entity-broadcast-range-percentage": {
    group: "Performance",
    label: "Entity visibility range",
    description: "Scales how far away entities are sent to clients.",
    control: int(10, 1000, "%"),
  },
  "max-tick-time": {
    group: "Performance",
    label: "Watchdog timeout",
    description:
      "Kill the server if one tick takes longer than this. -1 disables the watchdog.",
    control: int(-1, undefined, "ms"),
    danger:
      "Set too low, ordinary lag spikes become crashes. Set to -1, a hung tick hangs forever.",
  },
  "max-chained-neighbor-updates": {
    group: "Performance",
    label: "Redstone update limit",
    description:
      "Cap on chained block updates before the server gives up, so a redstone bomb cannot stall it.",
    control: int(),
  },
  "sync-chunk-writes": {
    group: "Performance",
    label: "Synchronous chunk writes",
    description:
      "Flush chunks to disk immediately. Safer if the machine loses power, slower on some disks.",
    control: bool(),
  },
  "region-file-compression": {
    group: "Performance",
    label: "Region compression",
    description: "Trade disk space against processor time when saving chunks.",
    control: enumOf(
      ["deflate", "Deflate (smaller)"],
      ["lz4", "LZ4 (faster)"],
      ["none", "None"],
    ),
  },
  "pause-when-empty-seconds": {
    group: "Performance",
    label: "Pause when empty after",
    description:
      "Stop ticking the world once nobody has been online this long. 0 never pauses.",
    control: int(0, undefined, "seconds"),
  },
  "enable-jmx-monitoring": {
    group: "Performance",
    label: "Expose JMX metrics",
    description: "Publish tick metrics for profilers to read.",
    control: bool(),
  },

  // --- Security ------------------------------------------------------------
  "enforce-secure-profile": {
    group: "Security",
    label: "Require signed chat",
    description:
      "Reject players without a Mojang-signed chat profile. Some chat-altering mods need this off.",
    control: bool(),
    danger: "Can lock out players whose clients do not sign chat.",
  },
  "op-permission-level": {
    group: "Security",
    label: "Operator level",
    description: "What operators are allowed to do.",
    control: enumOf(
      ["1", "1 — bypass spawn protection"],
      ["2", "2 — single-player commands"],
      ["3", "3 — player management"],
      ["4", "4 — everything, including stopping the server"],
    ),
  },
  "function-permission-level": {
    group: "Security",
    label: "Function permission level",
    description:
      "The permission level datapack functions and command blocks run at.",
    control: enumOf(
      ["1", "1 — bypass spawn protection"],
      ["2", "2 — single-player commands"],
      ["3", "3 — player management"],
      ["4", "4 — everything"],
    ),
  },
  "enable-command-block": {
    group: "Security",
    label: "Enable command blocks",
    description: "Whether command blocks run at all.",
    control: bool(),
  },
  "spawn-protection": {
    group: "Security",
    label: "Spawn protection radius",
    description:
      "Area around spawn that non-operators cannot build in. 0 turns it off.",
    control: int(0, undefined, "blocks"),
  },
  "log-ips": {
    group: "Security",
    label: "Log player addresses",
    description:
      "Write joining players' addresses to the log. Turn off to keep logs shareable.",
    control: bool(),
  },
};

/**
 * Keys that older files still carry and modern servers ignore. Recognised so
 * they can be labelled rather than left looking like something worth setting.
 */
export const RETIRED_KEYS: Record<string, string> = {
  "snooper-enabled": "No longer used — removed in 1.18.",
  "announce-player-achievements": "Replaced by the announceAdvancements game rule.",
  "max-build-height": "No longer used — removed in 1.17.",
  "resource-pack-hash": "Superseded by resource-pack-sha1.",
  "previews-chat": "Only existed in 1.19.1.",
};

/**
 * Keys that are only read when the world is first created, so changing them on
 * an existing server does nothing to what is already generated.
 */
export const CREATION_ONLY = new Set([
  "level-seed",
  "level-type",
  "generator-settings",
  "generate-structures",
  "initial-enabled-packs",
  "initial-disabled-packs",
]);
