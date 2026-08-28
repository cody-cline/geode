import { useState } from "react";
import { Loader2, Package } from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { useStore } from "../lib/store";
import type { Project } from "../lib/types";

import styles from "./ModpackInstallModal.module.css";

/**
 * Modpacks create an instance rather than installing into one, so this asks for
 * a name instead of a target.
 */
export default function ModpackInstallModal({
  project,
  versionId,
  onClose,
}: {
  project: Project;
  versionId?: string;
  onClose: () => void;
}) {
  const navigate = useStore((s) => s.navigate);
  const refreshInstances = useStore((s) => s.refreshInstances);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [name, setName] = useState(project.name);
  const [busy, setBusy] = useState(false);

  async function install() {
    setBusy(true);
    try {
      const instance = await api.installModpack(
        project.provider,
        project.id,
        versionId,
        name.trim() || project.name,
      );
      await refreshInstances();
      toast({
        kind: "success",
        title: `Installed ${instance.name}`,
        body: `Minecraft ${instance.mcVersion}${
          instance.loader !== "vanilla" ? ` · ${instance.loader}` : ""
        }`,
      });
      navigate({ view: "instance", id: instance.id });
      onClose();
    } catch (e) {
      // A partial import still leaves something usable, so refresh either way.
      await refreshInstances();
      reportError(`Could not install ${project.name}`, e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      title={`Install ${project.name}`}
      subtitle="Modpacks create their own instance, with the Minecraft version and loader the pack specifies."
      onClose={onClose}
      width={440}
      footer={
        <>
          <button className="btn" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button className="btn btn-primary" onClick={install} disabled={busy}>
            {busy ? <Loader2 size={14} className="spin" /> : <Package size={15} />}
            Create instance
          </button>
        </>
      }
    >
      <div>
        <label className="label" htmlFor="pack-name">
          Instance name
        </label>
        <input
          id="pack-name"
          className="input"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && !busy && install()}
          autoFocus
        />
        <p className="hint">
          The pack decides the Minecraft version, loader and mod list — those are
          read from its manifest during install.
        </p>
      </div>

      {!project.distributionAllowed && (
        <p className={styles.warn}>
          Some files in this pack may be marked no-third-party-download by their
          authors. Those are skipped and listed afterwards so you can grab them
          by hand.
        </p>
      )}
    </Modal>
  );
}
