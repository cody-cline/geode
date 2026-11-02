import { api, errorMessage } from "./api";
import { useStore } from "./store";
import type { Instance } from "./types";

/**
 * Launch an instance, doing the account check up front so the failure is a
 * one-line toast rather than a stack of download progress that dead-ends.
 */
export async function launchInstance(instance: Instance, offline = false) {
  const { accounts, toast, reportError, refreshInstances } = useStore.getState();

  if (accounts.length === 0) {
    toast({
      kind: "error",
      title: "No account",
      body: "Add a Microsoft or offline account before launching.",
    });
    return;
  }

  try {
    const result = await api.launchInstance(instance.id, undefined, offline);
    await refreshInstances();
    toast({ kind: "success", title: `Launching ${instance.name}` });
    // After the launch toast, not instead of it: the game is on its way either
    // way, and these are about what its server did or could not be asked.
    if (result.notices.length > 0) {
      toast({
        kind: "info",
        title: `About ${instance.name}`,
        body: result.notices.join(" "),
      });
    }
  } catch (e) {
    // A launch that could not sign in has a way past it: play as this account
    // offline, keeping its own name and UUID. Asked as a dialog rather than
    // taken quietly, and only when there is something to offer — the backend
    // answers that from the account's own state, so nothing here reads error
    // text to guess what went wrong.
    if (!offline && (await offlinePlayOffered(instance, errorMessage(e)))) {
      await refreshInstances();
      return;
    }
    reportError(`Could not launch ${instance.name}`, e);
    await refreshInstances();
  }
}

/** Raise the dialog if this account could play offline, and say whether it did. */
async function offlinePlayOffered(
  instance: Instance,
  reason: string,
): Promise<boolean> {
  let username: string | null = null;
  try {
    username = await api.offlinePlayOffer();
  } catch {
    // Nothing to add: the launch failure below is the news.
    return false;
  }
  if (!username) return false;
  useStore.getState().offerOfflinePlay({
    instanceId: instance.id,
    instanceName: instance.name,
    username,
    reason,
  });
  return true;
}

export async function stopInstance(instance: Instance) {
  const { reportError, refreshInstances } = useStore.getState();
  try {
    await api.stopInstance(instance.id);
    await refreshInstances();
  } catch (e) {
    reportError("Could not stop the game", e);
  }
}
