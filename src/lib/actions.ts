import { api } from "./api";
import { useStore } from "./store";
import type { Instance } from "./types";

/**
 * Launch an instance, doing the account check up front so the failure is a
 * one-line toast rather than a stack of download progress that dead-ends.
 */
export async function launchInstance(instance: Instance) {
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
    const result = await api.launchInstance(instance.id);
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
    reportError(`Could not launch ${instance.name}`, e);
    await refreshInstances();
  }
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
