import { useEffect, useState } from "react";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import {
  ArrowLeft,
  FolderOpen,
  Loader2,
  Play,
  Square,
  Clock,
  ExternalLink,
  Package,
} from "lucide-react";

import InstanceIcon from "../components/InstanceIcon";
import Browse from "./Browse";
import ContentTab from "./instance/ContentTab";
import LogsTab from "./instance/LogsTab";
import PackTab from "./instance/PackTab";
import SettingsTab from "./instance/SettingsTab";
import { api } from "../lib/api";
import { launchInstance, stopInstance } from "../lib/actions";
import { hashHue, relativeTime } from "../lib/format";
import { formatDuration, useStore } from "../lib/store";
import { LOADERS } from "../lib/types";

import styles from "./InstanceView.module.css";

const TABS = [
  { id: "content", label: "Content" },
  { id: "pack", label: "Pack" },
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
  // Browsing happens inside the instance rather than navigating to the global
  // Browse view, so the sidebar stays put and the destination stays obvious.
  const [browsing, setBrowsing] = useState(false);

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

  if (browsing) {
    return <Browse lockedInstance={instance} onDone={() => setBrowsing(false)} />;
  }

  const pack = instance.pack;

  const hero = pack?.bannerUrl ?? pack?.iconUrl ?? null;
  const hue = hashHue(instance.id);

  return (
    <>
      <div className={clsx(styles.hero, hero && styles.heroArt)}>
        {hero ? (
          <img className={styles.heroImg} src={hero} alt="" />
        ) : (
          <span
            className={styles.heroImg}
            style={{
              background: `linear-gradient(135deg, hsl(${hue} 44% 30%), hsl(${(hue + 55) % 360} 42% 18%))`,
            }}
          />
        )}
        <span className={styles.heroScrim} />

        <button
          className={styles.back}
          onClick={() => navigate({ view: "library" })}
          aria-label="Back to library"
        >
          <ArrowLeft size={17} />
        </button>

        <header className={styles.header}>
        <InstanceIcon instance={instance} size={64} radius={16} />

        <div className={styles.headText}>
          <h1 className={styles.name}>{instance.name}</h1>
          <div className={styles.meta}>
            <span>Minecraft {instance.mcVersion}</span>
            {instance.loader !== "vanilla" && (
              <span className="badge badge-accent">
                {loaderLabel} {instance.loaderVersion}
              </span>
            )}
            {pack &&
              (pack.pageUrl ? (
                <button
                  className={clsx("badge", styles.packLink)}
                  onClick={() => void openUrl(pack.pageUrl!)}
                  title={`Open ${pack.name} on ${pack.provider ?? "its page"}`}
                >
                  <Package size={10} />
                  {pack.name} {pack.version}
                  <ExternalLink size={9} />
                </button>
              ) : (
                <span className="badge">
                  <Package size={10} />
                  {pack.name} {pack.version}
                </span>
              ))}
          </div>
          {pack?.summary && <p className={styles.packSummary}>{pack.summary}</p>}
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
      </div>

      <nav className={styles.tabs}>
        {TABS.filter((t) => t.id !== "pack" || Boolean(pack?.projectId)).map((t) => (
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
        {active === "content" && (
          <ContentTab instance={instance} onBrowse={() => setBrowsing(true)} />
        )}
        {active === "pack" && <PackTab instance={instance} />}
        {active === "logs" && <LogsTab instanceId={id} />}
        {active === "settings" && <SettingsTab instance={instance} />}
      </div>
    </>
  );
}
