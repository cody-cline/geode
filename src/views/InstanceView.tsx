import { useEffect, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import {
  ArrowLeft,
  FolderOpen,
  Loader2,
  Play,
  Square,
  Clock,
  Package,
} from "lucide-react";

import InstanceIcon from "../components/InstanceIcon";
import ContentTab from "./instance/ContentTab";
import LogsTab from "./instance/LogsTab";
import SettingsTab from "./instance/SettingsTab";
import { api } from "../lib/api";
import { launchInstance, stopInstance } from "../lib/actions";
import { relativeTime } from "../lib/format";
import { formatDuration, useStore } from "../lib/store";
import { LOADERS } from "../lib/types";

import styles from "./InstanceView.module.css";

const TABS = [
  { id: "content", label: "Content" },
  { id: "logs", label: "Logs" },
  { id: "settings", label: "Settings" },
] as const;

type TabId = (typeof TABS)[number]["id"];

export default function InstanceView({ id, tab }: { id: string; tab?: string }) {
  const instances = useStore((s) => s.instances);
  const navigate = useStore((s) => s.navigate);
  const reportError = useStore((s) => s.reportError);
  const progress = useStore((s) => s.progress[`launch:${id}`]);
  const logs = useStore((s) => s.logs[id]);

  const [active, setActive] = useState<TabId>(
    TABS.some((t) => t.id === tab) ? (tab as TabId) : "content",
  );

  const instance = instances.find((i) => i.id === id);

  // The instance can disappear from under us (deleted in another view).
  useEffect(() => {
    if (instances.length > 0 && !instance) navigate({ view: "library" });
  }, [instances, instance, navigate]);

  if (!instance) {
    return (
      <div className={styles.loading}>
        <Loader2 size={20} className="spin" />
      </div>
    );
  }

  const loaderLabel = LOADERS.find((l) => l.id === instance.loader)?.label;
  const preparing = Boolean(progress) && !instance.running;

  async function openFolder() {
    try {
      const path = await api.instanceFolder(id);
      await revealItemInDir(path);
    } catch (e) {
      reportError("Could not open the folder", e);
    }
  }

  return (
    <>
      <header className={styles.header}>
        <button
          className={styles.back}
          onClick={() => navigate({ view: "library" })}
          aria-label="Back to library"
        >
          <ArrowLeft size={16} />
        </button>

        <InstanceIcon instance={instance} size={52} />

        <div className={styles.headText}>
          <h1 className={styles.name}>{instance.name}</h1>
          <div className={styles.meta}>
            <span>Minecraft {instance.mcVersion}</span>
            {instance.loader !== "vanilla" && (
              <span className="badge badge-accent">
                {loaderLabel} {instance.loaderVersion}
              </span>
            )}
            {instance.pack && (
              <span className="badge">
                <Package size={10} />
                {instance.pack.name} {instance.pack.version}
              </span>
            )}
          </div>
          <div className={styles.stats}>
            <span>
              <Clock size={11} />
              {instance.playSeconds > 0
                ? formatDuration(instance.playSeconds)
                : "Never played"}
            </span>
            {instance.lastPlayed && <span>Last played {relativeTime(instance.lastPlayed)}</span>}
          </div>
        </div>

        <div className={styles.headActions}>
          <button className="btn" onClick={openFolder}>
            <FolderOpen size={15} />
            Folder
          </button>
          {instance.running ? (
            <button
              className="btn btn-danger"
              onClick={() => void stopInstance(instance)}
            >
              <Square size={13} fill="currentColor" />
              Stop
            </button>
          ) : (
            <button
              className="btn btn-primary"
              onClick={() => void launchInstance(instance)}
              disabled={preparing}
            >
              {preparing ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <Play size={14} fill="currentColor" />
              )}
              {preparing ? "Preparing…" : "Play"}
            </button>
          )}
        </div>
      </header>

      <nav className={styles.tabs}>
        {TABS.map((t) => (
          <button
            key={t.id}
            className={clsx(styles.tab, active === t.id && styles.tabActive)}
            onClick={() => setActive(t.id)}
          >
            {t.label}
            {t.id === "logs" && logs && logs.length > 0 && (
              <span className={styles.tabCount}>{logs.length}</span>
            )}
          </button>
        ))}
      </nav>

      <div className={styles.body}>
        {active === "content" && <ContentTab instance={instance} />}
        {active === "logs" && <LogsTab instanceId={id} />}
        {active === "settings" && <SettingsTab instance={instance} />}
      </div>
    </>
  );
}
