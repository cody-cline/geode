import { useState } from "react";
import { Loader2, WifiOff } from "lucide-react";

import Modal from "./Modal";
import { launchInstance } from "../lib/actions";
import { useStore } from "../lib/store";
import type { OfflinePlayRequest } from "../lib/store";

import styles from "./OfflinePlayModal.module.css";

/**
 * The way past a sign-in that could not be renewed.
 *
 * A dialog and not a fallback: playing offline without being asked would tell
 * a player they are signed in when they are not. Both halves have to be on
 * screen before they choose — that their own character comes back, and that
 * online-mode servers will turn this session away, because the alternative is
 * learning the second half at a join screen.
 */
export default function OfflinePlayModal({
  request,
  onClose,
}: {
  request: OfflinePlayRequest;
  onClose: () => void;
}) {
  const instances = useStore((s) => s.instances);
  const toast = useStore((s) => s.toast);
  const [starting, setStarting] = useState(false);

  async function playOffline() {
    const instance = instances.find((i) => i.id === request.instanceId);
    if (!instance) {
      toast({
        kind: "error",
        title: `${request.instanceName} is no longer there`,
      });
      onClose();
      return;
    }
    setStarting(true);
    // The launch reports itself, success or failure, the same as any other.
    await launchInstance(instance, true);
    onClose();
  }

  return (
    <Modal
      title="Play offline?"
      subtitle={`Geode could not renew ${request.username}'s Microsoft sign-in.`}
      onClose={onClose}
      width={460}
      footer={
        <>
          <button className="btn" onClick={onClose} disabled={starting}>
            Not now
          </button>
          <button
            className="btn btn-primary"
            onClick={() => void playOffline()}
            disabled={starting}
          >
            {starting ? (
              <Loader2 size={14} className="spin" />
            ) : (
              <WifiOff size={14} />
            )}
            Play offline as {request.username}
          </button>
        </>
      }
    >
      <p className={styles.reason}>{request.reason}</p>

      <p className={styles.lead}>
        Minecraft knows you by UUID, not by name, so this plays{" "}
        <strong>{request.instanceName}</strong> as{" "}
        <strong>{request.username}</strong> with that account&rsquo;s own UUID.
        Your worlds open the character you left in them.
      </p>

      <p className={styles.caveat}>
        Single-player and LAN work. Servers with online mode enabled will reject
        this session — there is no sign-in behind it for them to verify. Sign in
        with Microsoft again, from Accounts, once you can reach it.
      </p>
    </Modal>
  );
}
