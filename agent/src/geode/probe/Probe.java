package geode.probe;

import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.ProtectionDomain;
import java.util.ArrayList;
import java.util.Collection;
import java.util.HashMap;
import java.util.IdentityHashMap;
import java.util.List;
import java.util.Map;
import java.util.jar.JarFile;

import org.objectweb.asm.ClassReader;
import org.objectweb.asm.ClassVisitor;
import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.MethodVisitor;
import org.objectweb.asm.Opcodes;

/**
 * Reads a Minecraft server's real command tree, from inside the server.
 *
 * A server sends every joining client the whole Brigadier tree, which is the
 * only complete account of what commands exist — mods and plugins register into
 * the same dispatcher as vanilla, and nothing in a mod jar declares them. But
 * that packet only exists in the PLAY state, so receiving it means joining the
 * world as a visible player. This reads the same tree from the dispatcher
 * directly, with nobody joining.
 *
 * <p>Brigadier is the hook point because it is not Mojang's obfuscated code: the
 * server ships it as a plain library ({@code com.mojang:brigadier}), so
 * {@code com.mojang.brigadier.CommandDispatcher} has that name on vanilla,
 * Paper, Forge, Fabric and Velocity alike. No per-version mappings are needed
 * to find the tree, and Brigadier's own public API walks it.
 *
 * <p><b>This runs inside somebody's server, so it never fails loudly.</b> A
 * broken probe must cost a dumb console at worst, never a server. Every entry
 * point catches {@link Throwable}; the transformer returns {@code null} (leave
 * the class alone) on any problem; the walk is bounded in both depth and node
 * count; and the only code on a server thread is {@link Hook#seen}, which is
 * built so that it cannot throw rather than catching what it might.
 */
public final class Probe {
    /** Bumped when the JSON layout changes, so the reader can tell. */
    private static final int FORMAT = 1;

    private static final String DISPATCHER = "com/mojang/brigadier/CommandDispatcher";

    /** Deep enough for any real command; shallow enough to never blow the stack. */
    private static final int MAX_DEPTH = 64;
    /** A stop on pathological trees. EssentialsX plus vanilla is a few thousand. */
    private static final int MAX_NODES = 250_000;

    private static boolean debug;
    private static Path out;

    private Probe() {}

    public static void premain(String args, Instrumentation inst) {
        try {
            start(args, inst);
        } catch (Throwable t) {
            // A probe that cannot start is a probe that does nothing. The
            // server carries on as though the flag had not been passed.
            moan("could not start", t);
        }
    }

    private static void start(String args, Instrumentation inst) throws Exception {
        String path = null;
        if (args != null) {
            for (String part : args.split(",")) {
                if ("debug".equals(part.trim())) {
                    debug = true;
                } else if (!part.trim().isEmpty()) {
                    path = part.trim();
                }
            }
        }
        if (path == null) {
            moan("no output path given", null);
            return;
        }
        out = new File(path).toPath();

        // The woven call has to reach Hook from whatever loader holds Brigadier.
        // Forge and Fabric both install transforming loaders, and neither owes
        // us parent delegation, so Hook goes on the bootstrap loader where
        // every loader can see it.
        //
        // Only Hook goes there. Appending this whole jar would put our copy of
        // ASM ahead of the server's own — Forge and Fabric both use ASM
        // heavily, and shadowing it would be a spectacular way to break a
        // server we were only trying to read.
        if (!hookOnBootstrap(inst)) {
            return;
        }

        inst.addTransformer(new Weaver(), false);
        Thread dumper = new Thread(new Dumper(), "geode-command-probe");
        dumper.setDaemon(true); // must never be the reason a JVM stays up
        dumper.setPriority(Thread.MIN_PRIORITY);
        dumper.start();
    }

    /** Put {@link Hook} where every class loader can see it, and prove it. */
    private static boolean hookOnBootstrap(Instrumentation inst) {
        File tmp = null;
        try {
            InputStream src = Probe.class.getResourceAsStream("/geode-probe-hook.jar");
            if (src == null) {
                moan("the agent jar has no hook jar inside it", null);
                return false;
            }
            tmp = File.createTempFile("geode-probe-hook", ".jar");
            tmp.deleteOnExit();
            OutputStream dst = Files.newOutputStream(tmp.toPath());
            try {
                byte[] buf = new byte[8192];
                int n;
                while ((n = src.read(buf)) > 0) {
                    dst.write(buf, 0, n);
                }
            } finally {
                dst.close();
                src.close();
            }
            inst.appendToBootstrapClassLoaderSearch(new JarFile(tmp));

            // Do not take the append on trust: if Hook is not actually
            // reachable from the bootstrap loader, weaving a call to it would
            // turn every command registration into a NoClassDefFoundError.
            Class.forName("geode.probe.Hook", false, null);
            return true;
        } catch (Throwable t) {
            moan("could not publish the hook class", t);
            return false;
        }
    }

    /** Prepends {@code Hook.seen(this)} to {@code CommandDispatcher.register}. */
    private static final class Weaver implements ClassFileTransformer {
        public byte[] transform(ClassLoader loader, String name, Class<?> beingRedefined,
                                ProtectionDomain domain, byte[] bytes) {
            try {
                if (!DISPATCHER.equals(name) || bytes == null) {
                    return null;
                }
                byte[] woven = weave(bytes);
                // Returning null means "leave it exactly as it was", which is
                // the right answer whenever we are not certain we improved it.
                return woven;
            } catch (Throwable t) {
                moan("could not weave the dispatcher", t);
                return null;
            }
        }
    }

    private static byte[] weave(byte[] bytes) {
        ClassReader reader = new ClassReader(bytes);
        // COMPUTE_MAXS only, deliberately. The injected sequence has no
        // branches, so ASM shifts the existing stack map frames itself and no
        // frame has to be recomputed — COMPUTE_FRAMES would re-derive every
        // frame in the class and needs to load types to merge them, which is
        // exactly the kind of work that goes wrong inside a modded loader.
        ClassWriter writer = new ClassWriter(reader, ClassWriter.COMPUTE_MAXS);
        final boolean[] hit = {false};

        reader.accept(new ClassVisitor(Opcodes.ASM9, writer) {
            @Override
            public MethodVisitor visitMethod(int access, String name, String descriptor,
                                             String signature, String[] exceptions) {
                MethodVisitor mv = super.visitMethod(access, name, descriptor, signature, exceptions);
                // `register` catches a server building its command set. `getRoot`
                // catches one that never calls register at all: Velocity creates a
                // dispatcher, takes its root, and adds nodes to that directly.
                boolean interesting = ("register".equals(name) || "getRoot".equals(name))
                        && (access & Opcodes.ACC_STATIC) == 0
                        && (access & Opcodes.ACC_ABSTRACT) == 0;
                if (mv == null || !interesting) {
                    return mv;
                }
                hit[0] = true;
                return new MethodVisitor(Opcodes.ASM9, mv) {
                    @Override
                    public void visitCode() {
                        super.visitCode();
                        super.visitVarInsn(Opcodes.ALOAD, 0);
                        super.visitMethodInsn(Opcodes.INVOKESTATIC, "geode/probe/Hook",
                                "seen", "(Ljava/lang/Object;)V", false);
                    }
                };
            }
        }, 0);

        // Never hand back a rewritten class we did not actually change.
        return hit[0] ? writer.toByteArray() : null;
    }

    /**
     * Writes the tree out whenever it changes, then stops.
     *
     * Registration is not finished when the server says "Done": Bukkit syncs
     * plugin commands into the dispatcher after plugins enable, and a plugin
     * may register later still. So this keeps looking for a while, and rewrites
     * whenever what it reads differs from what it last wrote.
     */
    private static final class Dumper implements Runnable {
        public void run() {
            String written = null;
            int writtenNodes = -1;
            int lastShape = -1;
            long started = System.currentTimeMillis();

            for (int tick = 0; tick < 400; tick++) {
                long age = System.currentTimeMillis() - started;
                if (age > 900_000L) { // fifteen minutes, then give up
                    return;
                }
                try {
                    // Briskly while the server is starting, because the console
                    // becomes usable the moment it is up and the tree should be
                    // there by then; slowly afterwards, when the only thing
                    // left to catch is a plugin registering late.
                    Thread.sleep(age < 30_000L ? 500L : 5000L);
                } catch (InterruptedException e) {
                    return;
                }

                try {
                    List<Object> roots = roots();
                    if (roots.isEmpty()) {
                        continue;
                    }
                    // A cheap change detector, so the full walk is rare. The
                    // early forced walks catch a plugin adding a subcommand to
                    // a command that already existed, which leaves the counts
                    // alone but changes the tree.
                    int shape = 0;
                    for (Object root : roots) {
                        shape = shape * 31 + childrenOf(root).size();
                    }
                    boolean worthWalking = shape != lastShape
                            || tick == 2 || tick == 20 || tick == 60 || tick == 90;
                    if (!worthWalking) {
                        continue;
                    }
                    lastShape = shape;

                    // Whichever dispatcher holds more. On a Bukkit server the
                    // fuller one is the second, built after plugins enabled;
                    // taking the larger also means a library that keeps a small
                    // private dispatcher of its own can never win.
                    String best = null;
                    int bestNodes = -1;
                    for (Object root : roots) {
                        Map<Object, List<String>> paths = pathsFrom(root);
                        if (paths.size() <= bestNodes) {
                            continue;
                        }
                        String json = render(root, paths);
                        if (json != null) {
                            best = json;
                            bestNodes = paths.size();
                        }
                    }

                    // Never replace a fuller tree with a thinner one: a reload
                    // rebuilding the dispatcher would otherwise be caught
                    // half-populated and lose commands that do still exist.
                    if (best != null && bestNodes >= writtenNodes && !best.equals(written)) {
                        if (write(best)) {
                            written = best;
                            writtenNodes = bestNodes;
                        }
                    }
                } catch (Throwable t) {
                    // Including ConcurrentModificationException: the server is
                    // still registering and we read mid-change. Next tick.
                    moan("could not read the tree this time", t);
                }
            }
        }
    }

    /** The root of every dispatcher the hook has seen, newest first. */
    private static List<Object> roots() throws Exception {
        // Reflection rather than a direct reference: the Hook the woven code
        // writes to is the bootstrap one, and a direct reference here would
        // bind to whichever copy this class loader sees.
        Class<?> hook = Class.forName("geode.probe.Hook", false, null);
        Object latest = hook.getField("latest").get(null);
        Object first = hook.getField("first").get(null);

        List<Object> out = new ArrayList<Object>(2);
        for (Object dispatcher : new Object[] {latest, first}) {
            if (dispatcher == null) {
                continue;
            }
            Object root = call(dispatcher, "getRoot");
            if (root == null) {
                continue;
            }
            boolean already = false;
            for (Object seen : out) {
                if (seen == root) {
                    already = true;
                    break;
                }
            }
            if (!already) {
                out.add(root);
            }
        }
        return out;
    }

    // ---------------------------------------------------------------------
    // Walking the tree, through Brigadier's own public API
    // ---------------------------------------------------------------------

    /**
     * Look ups are cached because a walk makes three of them per node, and a
     * Paper server with a big plugin has five thousand nodes — reflective
     * method lookup is the expensive half of that, and the answer never
     * changes for a given class.
     */
    private static final Map<String, java.lang.reflect.Method> METHODS =
            new java.util.concurrent.ConcurrentHashMap<String, java.lang.reflect.Method>();

    private static Object call(Object target, String method) {
        try {
            Class<?> owner = target.getClass();
            String key = owner.getName() + "#" + method;
            java.lang.reflect.Method m = METHODS.get(key);
            if (m == null) {
                m = owner.getMethod(method);
                m.setAccessible(true);
                METHODS.put(key, m);
            }
            return m.invoke(target);
        } catch (Throwable t) {
            return null;
        }
    }

    @SuppressWarnings("unchecked")
    private static Collection<Object> childrenOf(Object node) {
        Object kids = call(node, "getChildren");
        if (!(kids instanceof Collection)) {
            return new ArrayList<Object>();
        }
        // Copied straight away: getChildren hands back a live view, and the
        // server may be adding to it while we look.
        return new ArrayList<Object>((Collection<Object>) kids);
    }

    /**
     * Paths to every node reachable through children, so a redirect can be
     * written as the path it points at.
     *
     * Redirects are why this is needed: /execute redirects to the root and /tp
     * to /teleport, so following them while walking would never terminate. The
     * generated vanilla report writes a path array instead, and so does this.
     */
    private static Map<Object, List<String>> pathsFrom(Object root) {
        Map<Object, List<String>> paths = new IdentityHashMap<Object, List<String>>();
        List<Object> queue = new ArrayList<Object>();
        List<List<String>> queuePaths = new ArrayList<List<String>>();
        queue.add(root);
        queuePaths.add(new ArrayList<String>());
        int seen = 0;
        while (!queue.isEmpty() && seen < MAX_NODES) {
            Object node = queue.remove(queue.size() - 1);
            List<String> here = queuePaths.remove(queuePaths.size() - 1);
            if (paths.containsKey(node)) {
                continue; // a shared node, reached twice
            }
            paths.put(node, here);
            seen++;
            if (here.size() >= MAX_DEPTH) {
                continue;
            }
            for (Object child : childrenOf(node)) {
                Object name = call(child, "getName");
                List<String> next = new ArrayList<String>(here);
                next.add(name == null ? "?" : String.valueOf(name));
                queue.add(child);
                queuePaths.add(next);
            }
        }
        return paths;
    }

    private static String render(Object root, Map<Object, List<String>> paths) {
        StringBuilder sb = new StringBuilder(1 << 16);
        int[] budget = {MAX_NODES};
        sb.append('{');
        sb.append("\"_probe\":{\"format\":").append(FORMAT)
          .append(",\"nodes\":").append(paths.size()).append('}');
        sb.append(",\"type\":\"root\"");
        appendChildren(sb, root, paths, 0, budget);
        sb.append('}');
        return sb.toString();
    }

    private static void appendChildren(StringBuilder sb, Object node,
                                       Map<Object, List<String>> paths, int depth, int[] budget) {
        if (depth >= MAX_DEPTH || budget[0] <= 0) {
            return;
        }
        Collection<Object> kids = childrenOf(node);
        if (kids.isEmpty()) {
            return;
        }
        sb.append(",\"children\":{");
        boolean first = true;
        for (Object child : kids) {
            if (budget[0]-- <= 0) {
                break;
            }
            Object nameObj = call(child, "getName");
            String name = nameObj == null ? null : String.valueOf(nameObj);
            if (name == null) {
                continue;
            }
            if (!first) {
                sb.append(',');
            }
            first = false;
            quote(sb, name);
            sb.append(":{");
            appendNode(sb, child, paths, depth, budget);
            sb.append('}');
        }
        sb.append('}');
    }

    private static void appendNode(StringBuilder sb, Object node,
                                   Map<Object, List<String>> paths, int depth, int[] budget) {
        boolean argument = isArgument(node);
        sb.append("\"type\":\"").append(argument ? "argument" : "literal").append('"');
        if (call(node, "getCommand") != null) {
            sb.append(",\"executable\":true");
        }
        if (argument) {
            appendParser(sb, node);
        }
        Object redirect = call(node, "getRedirect");
        if (redirect != null) {
            List<String> target = paths.get(redirect);
            if (target != null) {
                sb.append(",\"redirect\":[");
                for (int i = 0; i < target.size(); i++) {
                    if (i > 0) {
                        sb.append(',');
                    }
                    quote(sb, target.get(i));
                }
                sb.append(']');
            }
        }
        appendChildren(sb, node, paths, depth + 1, budget);
    }

    private static boolean isArgument(Object node) {
        for (Class<?> c = node.getClass(); c != null; c = c.getSuperclass()) {
            if ("com.mojang.brigadier.tree.ArgumentCommandNode".equals(c.getName())) {
                return true;
            }
        }
        return false;
    }

    /**
     * What kind of argument this is, as far as can honestly be said.
     *
     * Brigadier's own six primitives are named outright: they live in the
     * unobfuscated library, so their class names mean the same thing
     * everywhere. Minecraft's argument types are a different matter — their
     * class names are obfuscated on vanilla and intermediary on Fabric — so
     * the class name is reported as-is under a different key and the reader
     * decides whether it recognises it. Guessing "minecraft:entity" from a
     * class called {@code cy$a} is not something to do on a server console.
     */
    private static void appendParser(StringBuilder sb, Object node) {
        Object type = call(node, "getType");
        if (type == null) {
            return;
        }
        String cls = type.getClass().getName();
        String brigadier = BRIGADIER_PARSERS.get(cls);
        if (brigadier != null) {
            sb.append(",\"parser\":\"").append(brigadier).append('"');
        } else {
            sb.append(",\"parserClass\":");
            quote(sb, cls);
        }
    }

    private static final Map<String, String> BRIGADIER_PARSERS = new HashMap<String, String>();
    static {
        String p = "com.mojang.brigadier.arguments.";
        BRIGADIER_PARSERS.put(p + "BoolArgumentType", "brigadier:bool");
        BRIGADIER_PARSERS.put(p + "DoubleArgumentType", "brigadier:double");
        BRIGADIER_PARSERS.put(p + "FloatArgumentType", "brigadier:float");
        BRIGADIER_PARSERS.put(p + "IntegerArgumentType", "brigadier:integer");
        BRIGADIER_PARSERS.put(p + "LongArgumentType", "brigadier:long");
        BRIGADIER_PARSERS.put(p + "StringArgumentType", "brigadier:string");
    }

    private static void quote(StringBuilder sb, String s) {
        sb.append('"');
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"':  sb.append("\\\""); break;
                case '\\': sb.append("\\\\"); break;
                case '\n': sb.append("\\n"); break;
                case '\r': sb.append("\\r"); break;
                case '\t': sb.append("\\t"); break;
                default:
                    if (c < 0x20 || c == 0x7f) {
                        sb.append(String.format("\\u%04x", (int) c));
                    } else {
                        sb.append(c);
                    }
            }
        }
        sb.append('"');
    }

    /** Replace the file in one step, so a reader never sees half a tree. */
    private static boolean write(String json) {
        try {
            Path parent = out.getParent();
            if (parent != null) {
                Files.createDirectories(parent);
            }
            Path tmp = out.resolveSibling(out.getFileName() + ".tmp");
            Files.write(tmp, json.getBytes("UTF-8"));
            try {
                Files.move(tmp, out, StandardCopyOption.ATOMIC_MOVE,
                        StandardCopyOption.REPLACE_EXISTING);
            } catch (IOException atomicUnsupported) {
                Files.move(tmp, out, StandardCopyOption.REPLACE_EXISTING);
            }
            return true;
        } catch (Throwable t) {
            moan("could not write the tree", t);
            return false;
        }
    }

    /**
     * Complain only to whoever asked for complaints.
     *
     * Silent by default and on stderr when not: the server's stdout is a
     * console somebody is reading, and the whole point of this is to stop
     * cluttering it.
     */
    private static void moan(String what, Throwable t) {
        if (!debug) {
            return;
        }
        try {
            System.err.println("[geode-probe] " + what);
            if (t != null) {
                t.printStackTrace();
            }
        } catch (Throwable ignored) {
            // Reporting a problem must not become one.
        }
    }
}
