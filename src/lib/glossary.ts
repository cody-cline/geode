/**
 * The words Geode invents, defined once.
 *
 * Every one of these is currently explained in exactly one view and nowhere
 * else: what an instance is lives in the Library's empty state, what a plan is
 * in the Plans lede, what a share is on the Share tab. That is good help while
 * you are on the screen and no help at all afterwards — and the empty-state
 * ones disappear for good the moment the list stops being empty.
 *
 * So the definitions live here and the screens read them. `short` is the
 * sentence that defines the thing, which is what a lede and a glossary row
 * both want; `more` is what the screen with room for it adds. A screen
 * composing both gets the paragraph it had before, out of one copy, and
 * nothing can drift from the glossary that quotes it.
 *
 * Only words this app invents or gives a particular meaning to. "Mod",
 * "loader" and "world" are Minecraft's and belong to the player already —
 * explaining those would be the kind of padding that makes a glossary
 * something nobody reads twice.
 */
export interface Term {
  /** The word as the interface prints it. */
  term: string;
  /** The one sentence that defines it. */
  short: string;
  /** What a screen with room for it adds, or `null` where there is no more. */
  more: string | null;
  /** Where in the app this is the thing you are looking at. */
  seeAlso?: string;
}

export const GLOSSARY: Term[] = [
  {
    term: "Instance",
    short: "One Minecraft install: its own version, mods, and worlds.",
    more: "Make as many as you like — they share downloads, so extras are nearly free on disk.",
    seeAlso: "Library",
  },
  {
    term: "Plan",
    short:
      "A list of mods with no Minecraft version and no loader attached.",
    more: "Geode works out afterwards which loader and version can run all of them.",
    seeAlso: "Plans",
  },
  {
    term: "Server",
    short:
      "A server running on this machine: anyone on your own network can join it, and friends further away once its port is open to the internet.",
    more: "Geode downloads the software, sets it up, and gives you a console to start and stop it from.",
    seeAlso: "Servers",
  },
  // Note for anyone editing the three above: the Library, Plans and Servers
  // screens render these same strings, as their lede and inside their empty
  // state. Changing one changes what those screens say, which is the point —
  // there is one copy — but it is not only a glossary edit.
  {
    term: "Pack",
    short:
      "An instance packed into one file — a .geodepack — that somebody else can open to get the same setup.",
    more: "Mods that a platform can name travel as a reference rather than as bytes, so the person opening it downloads them from Modrinth or CurseForge with their own account. Anything no platform names is carried inside the file.",
    seeAlso: "an instance's Share menu",
  },
  {
    term: "Sharing a server",
    short:
      "A running server handing its own mods to the players who join it, over a link and a password you give them.",
    more: "Nothing is uploaded anywhere: the players' copies of Geode fetch the pack from your server directly, and get the change the next time they press Play.",
    seeAlso: "a server's Share tab",
  },
  {
    term: "geode:// link",
    short:
      "A link that opens Geode rather than a web page — to join a shared server, or to install a pack somebody put online.",
    more: "A join link carries an address and the server's share code; a pack link carries an address and a checksum of the file. Either way, clicking one shows you what it would do before anything is installed or joined.",
  },
  {
    term: "Host",
    short:
      "Another machine running Geode with no window — a spare computer, or a container — that keeps servers up when this one is off.",
    more: "The app talks to it over the network. A server can be moved or copied between this machine and a host.",
    seeAlso: "Settings → Server hosts",
  },
  {
    term: "Group",
    short:
      "A name you give several instances so the Library lists them together under it.",
    more: "Nothing else changes: an instance's group is a label for your own sorting.",
    seeAlso: "an instance's Settings tab",
  },
  {
    term: "Added by hand",
    short:
      "A file Geode found in a folder but did not install, so it knows nothing about where it came from.",
    more: "It works exactly like any other — it just cannot be checked for updates, because nothing says which project it is. Identify matches it against the platforms to find out.",
    seeAlso: "a Content tab",
  },
  {
    term: "Client mods",
    short:
      "Mods a server keeps for its players and never loads itself.",
    more: "A minimap or a shader does nothing on a server and can crash it on boot, so these sit in a folder no loader reads — and still travel to every player who joins.",
    seeAlso: "a server's Content tab",
  },
];

/** One term by name, for a screen quoting its own definition. */
export function term(name: string): Term {
  const found = GLOSSARY.find((t) => t.term === name);
  // Cannot be missing: callers pass a literal from the list above, and the
  // test beside this file fails if a screen quotes a term that has gone.
  return found!;
}
