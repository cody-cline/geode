import { useMemo, useState } from "react";
import clsx from "clsx";
import {
  Boxes,
  ClipboardList,
  Compass,
  Loader2,
  Play,
  Plus,
  Server as ServerIcon,
  Settings as SettingsIcon,
  UserRound,
} from "lucide-react";

import AccountsModal from "./AccountsModal";
import InstanceIcon from "./InstanceIcon";
import NewInstanceModal from "./NewInstanceModal";
import NewServerModal from "./NewServerModal";
import { hashHue, initials } from "../lib/format";
import { useStore } from "../lib/store";
import type { Route } from "../lib/store";

import styles from "./Sidebar.module.css";

export default function Sidebar() {
  const route = useStore((s) => s.route);
  const navigate = useStore((s) => s.navigate);
  const instances = useStore((s) => s.instances);
  const servers = useStore((s) => s.servers);
  const accounts = useStore((s) => s.accounts);
  const progress = useStore((s) => s.progress);

  const [creating, setCreating] = useState(false);
  const [creatingServer, setCreatingServer] = useState(false);
  const [managingAccounts, setManagingAccounts] = useState(false);

  const active = accounts.find((a) => a.active);

  // Most-recent first, capped: the sidebar is a shortcut, the library is the
  // complete list.
  const recent = useMemo(() => instances.slice(0, 8), [instances]);

  const isActive = (r: Route) =>
    r.view === route.view &&
    (r.view !== "instance" ||
      (route.view === "instance" && r.id === route.id));

  return (
    <aside className={styles.sidebar}>
      <div className={styles.brand}>
        <svg viewBox="0 0 512 512" className={styles.mark} aria-hidden="true">
          <path
            d="M256 60 L420 154 L420 358 L256 452 L92 358 L92 154 Z"
            fill="none"
            stroke="currentColor"
            strokeWidth="34"
            strokeLinejoin="round"
            opacity="0.35"
          />
          <path d="M256 132 L322 240 L256 300 L190 240 Z" fill="currentColor" />
          <path
            d="M190 240 L256 300 L232 386 L162 306 Z"
            fill="currentColor"
            opacity="0.75"
          />
          <path
            d="M322 240 L256 300 L280 386 L350 306 Z"
            fill="currentColor"
            opacity="0.55"
          />
        </svg>
        <span className={styles.wordmark}>Geode</span>
      </div>

      <nav className={styles.nav}>
        <button
          className={clsx(styles.navItem, isActive({ view: "library" }) && styles.navActive)}
          onClick={() => navigate({ view: "library" })}
        >
          <Boxes size={17} />
          Library
        </button>
        <button
          className={clsx(styles.navItem, isActive({ view: "browse" }) && styles.navActive)}
          onClick={() => navigate({ view: "browse" })}
        >
          <Compass size={17} />
          Browse
        </button>
        <button
          className={clsx(
            styles.navItem,
            (route.view === "plans" || route.view === "plan") && styles.navActive,
          )}
          onClick={() => navigate({ view: "plans" })}
        >
          <ClipboardList size={17} />
          Plans
        </button>
        <button
          className={clsx(
            styles.navItem,
            (route.view === "servers" || route.view === "server") &&
              styles.navActive,
          )}
          onClick={() => navigate({ view: "servers" })}
        >
          <ServerIcon size={17} />
          Servers
        </button>
      </nav>

      <div className={styles.sectionHead}>
        <span>Instances</span>
        <button
          className={styles.addBtn}
          onClick={() => setCreating(true)}
          title="New instance"
          aria-label="New instance"
        >
          <Plus size={15} />
        </button>
      </div>

      <div className={styles.sections}>
      <div className={styles.instanceList}>
        {recent.length === 0 && (
          <p className={styles.emptyHint}>Nothing here yet — create one.</p>
        )}
        {recent.map((instance) => {
          const busy = progress[`launch:${instance.id}`];
          return (
            <button
              key={instance.id}
              className={clsx(
                styles.instanceItem,
                isActive({ view: "instance", id: instance.id }) && styles.navActive,
              )}
              onClick={() => navigate({ view: "instance", id: instance.id })}
              title={instance.name}
            >
              <InstanceIcon instance={instance} size={22} />
              <span className={styles.instanceName}>{instance.name}</span>
              {instance.running ? (
                <Play size={12} className={styles.runningDot} fill="currentColor" />
              ) : busy ? (
                <Loader2 size={12} className="spin" />
              ) : null}
            </button>
          );
        })}
      </div>

      <div className={styles.sectionHead}>
        <span>Servers</span>
        <button
          className={styles.addBtn}
          onClick={() => setCreatingServer(true)}
          title="New server"
          aria-label="New server"
        >
          <Plus size={15} />
        </button>
      </div>

      <div className={styles.instanceList}>
        {servers.length === 0 && (
          <p className={styles.emptyHint}>No servers yet.</p>
        )}
        {servers.slice(0, 8).map((server) => (
          <button
            key={server.id}
            className={clsx(
              styles.instanceItem,
              route.view === "server" &&
                route.id === server.id &&
                styles.navActive,
            )}
            onClick={() => navigate({ view: "server", id: server.id })}
            title={server.name}
          >
            <span className={styles.serverDot} data-live={server.running} />
            <span className={styles.instanceName}>{server.name}</span>
          </button>
        ))}
      </div>
      </div>

      <div className={styles.footer}>
        <button className={styles.account} onClick={() => setManagingAccounts(true)}>
          {/* Avatars are generated locally from the profile id. Fetching skin
              heads would mean sending every account UUID to a third party. */}
          <span
            className={styles.avatar}
            style={
              active
                ? {
                    background: `linear-gradient(135deg, hsl(${hashHue(active.id)} 55% 42%), hsl(${(hashHue(active.id) + 40) % 360} 55% 30%))`,
                    color: "#fff",
                  }
                : undefined
            }
          >
            {active ? (
              <span className={styles.avatarText}>{initials(active.username)}</span>
            ) : (
              <UserRound size={15} />
            )}
          </span>
          <span className={styles.accountText}>
            <span className={styles.accountName}>
              {active ? active.username : "No account"}
            </span>
            <span className={styles.accountKind}>
              {active
                ? active.kind === "microsoft"
                  ? "Microsoft"
                  : "Offline"
                : "Sign in to play"}
            </span>
          </span>
        </button>

        <button
          className={clsx(styles.navItem, isActive({ view: "settings" }) && styles.navActive)}
          onClick={() => navigate({ view: "settings" })}
        >
          <SettingsIcon size={17} />
          Settings
        </button>
      </div>

      {creating && <NewInstanceModal onClose={() => setCreating(false)} />}
      {creatingServer && (
        <NewServerModal
          onClose={() => setCreatingServer(false)}
          onCreated={(server) => {
            setCreatingServer(false);
            navigate({ view: "server", id: server.id });
          }}
        />
      )}
      {managingAccounts && (
        <AccountsModal onClose={() => setManagingAccounts(false)} />
      )}
    </aside>
  );
}
