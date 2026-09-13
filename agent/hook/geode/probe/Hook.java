package geode.probe;

/**
 * The one thing woven into the server: somewhere to put the command dispatchers
 * as they turn up.
 *
 * There is usually more than one. CraftBukkit's {@code syncCommands()} throws
 * away the dispatcher the server booted with and builds a fresh one once
 * plugins have enabled, copying the vanilla nodes across and adding every
 * plugin command to the new one. Keeping only the first is how a Spigot server
 * reports vanilla's ninety commands and none of EssentialsX's — so the first
 * and the most recent are both kept, and the reader takes whichever turns out
 * to hold more.
 *
 * This class is the only part of the probe that runs on the server's own
 * threads, so it is built so that the call cannot fail rather than so that
 * failures are caught:
 *
 * <ul>
 *   <li>It has no static initialiser — bare fields with no initialiser
 *       expressions compile to no {@code <clinit>} at all — so the woven call
 *       site cannot raise {@code ExceptionInInitializerError}.
 *   <li>It is loaded by the bootstrap class loader, so it is visible from
 *       whatever loader the server gave Brigadier, and the call site cannot
 *       raise {@code NoClassDefFoundError}. The agent verifies that before it
 *       weaves anything.
 *   <li>The body is at most two reference writes. It allocates nothing, takes
 *       no lock, touches no I/O and calls nothing, so there is no statement in
 *       it that can throw.
 * </ul>
 *
 * Everything expensive — walking the tree, building JSON, writing a file —
 * happens on the probe's own daemon thread, never here.
 */
public final class Hook {
    /** The dispatcher the server started with. */
    public static volatile Object first;

    /** The most recent one, which on a Bukkit server is the one that matters. */
    public static volatile Object latest;

    private Hook() {}

    /** Woven in at the top of {@code CommandDispatcher.register} and {@code getRoot}. */
    public static void seen(Object candidate) {
        if (first == null) {
            first = candidate;
        }
        // `getRoot` is called whenever a command runs, so the common case is a
        // volatile read that changes nothing — cheaper than writing every time.
        if (latest != candidate) {
            latest = candidate;
        }
    }
}
