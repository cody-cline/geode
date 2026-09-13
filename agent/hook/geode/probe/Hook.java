package geode.probe;

/**
 * The one thing woven into the server: a place to put the command dispatcher.
 *
 * This class is the only part of the probe that runs on the server's own
 * threads, so it is built so that the call cannot fail rather than so that
 * failures are caught:
 *
 * <ul>
 *   <li>It has no static initialiser — a bare field with no initialiser
 *       expression compiles to no {@code <clinit>} at all — so the woven call
 *       site cannot raise {@code ExceptionInInitializerError}.
 *   <li>It is loaded by the bootstrap class loader, so it is visible from
 *       whatever loader the server gave Brigadier, and the call site cannot
 *       raise {@code NoClassDefFoundError}. The agent verifies that before it
 *       weaves anything.
 *   <li>The body is a single reference write. It allocates nothing, takes no
 *       lock, touches no I/O and calls nothing.
 * </ul>
 *
 * Everything expensive — walking the tree, building JSON, writing a file —
 * happens on the probe's own daemon thread, never here.
 */
public final class Hook {
    /** The first dispatcher a command was registered on. */
    public static volatile Object dispatcher;

    private Hook() {}

    /** Woven in at the top of {@code CommandDispatcher.register}. */
    public static void seen(Object candidate) {
        if (dispatcher == null) {
            dispatcher = candidate;
        }
    }
}
