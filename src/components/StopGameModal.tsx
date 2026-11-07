import { Square } from "lucide-react";

import Modal from "./Modal";
import { stopInstance } from "../lib/actions";
import type { Instance } from "../lib/types";

import styles from "./StopGameModal.module.css";

/**
 * The question Stop has to ask, wherever Stop is.
 *
 * `stopInstance` ends the JVM with SIGKILL — no signal the game can handle, no
 * shutdown hook, nothing flushed — so the press costs whatever has happened
 * since Minecraft last saved by itself, and a world is the one thing with no
 * upstream to fetch it back from.
 *
 * A component rather than the same dialog written on each screen, because two
 * screens call `stopInstance` and the first version of this warned on one of
 * them. A copy of the warning is a copy that goes stale, and worse, the screen
 * without it quietly teaches that the press is free.
 */
export default function StopGameModal({
  instance,
  onClose,
}: {
  instance: Instance;
  onClose: () => void;
}) {
  return (
    <Modal
      title={`Stop ${instance.name}?`}
      subtitle="Minecraft is not asked to save first, because there is nothing here to ask."
      onClose={onClose}
      width={460}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Keep playing
          </button>
          <button
            className="btn btn-danger"
            onClick={() => {
              onClose();
              void stopInstance(instance);
            }}
          >
            <Square size={13} fill="currentColor" />
            Stop the game
          </button>
        </>
      }
    >
      <p className={styles.note}>
        Geode ends the process outright. A server can be told to{" "}
        <code>save-all flush</code> and waited for; a client has no console to
        type that at, so whatever has happened since Minecraft last saved by
        itself goes with it — and a world is the one thing with no upstream to
        fetch it back from. Quit from inside the game to save first. Stop here
        when the game has stopped responding and that is not an option.
      </p>
    </Modal>
  );
}
