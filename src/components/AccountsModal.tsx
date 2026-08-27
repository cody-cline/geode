import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { Check, Copy, ExternalLink, Loader2, Trash2, UserPlus } from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { hashHue, initials } from "../lib/format";
import { useStore } from "../lib/store";
import type { DeviceCodeStart } from "../lib/types";

import styles from "./AccountsModal.module.css";

export default function AccountsModal({ onClose }: { onClose: () => void }) {
  const accounts = useStore((s) => s.accounts);
  const config = useStore((s) => s.config);
  const navigate = useStore((s) => s.navigate);
  const refreshAccounts = useStore((s) => s.refreshAccounts);
  const toast = useStore((s) => s.toast);
  const reportError = useStore((s) => s.reportError);

  const [pending, setPending] = useState<DeviceCodeStart | null>(null);
  const [signingIn, setSigningIn] = useState(false);
  const [copied, setCopied] = useState(false);
  const [offlineName, setOfflineName] = useState("");
  const [addingOffline, setAddingOffline] = useState(false);

  async function startMicrosoftLogin() {
    setSigningIn(true);
    try {
      const start = await api.beginMicrosoftLogin();
      setPending(start);
      await openUrl(start.verificationUri).catch(() => {});

      // This resolves only once the user finishes in their browser.
      const account = await api.completeMicrosoftLogin(start);
      await refreshAccounts();
      toast({ kind: "success", title: `Signed in as ${account.username}` });
      setPending(null);
    } catch (e) {
      reportError("Microsoft sign-in failed", e);
      setPending(null);
    } finally {
      setSigningIn(false);
    }
  }

  async function addOffline() {
    if (!offlineName.trim()) return;
    setAddingOffline(true);
    try {
      const account = await api.addOfflineAccount(offlineName.trim());
      await refreshAccounts();
      setOfflineName("");
      toast({ kind: "success", title: `Added ${account.username}` });
    } catch (e) {
      reportError("Could not add that account", e);
    } finally {
      setAddingOffline(false);
    }
  }

  async function select(id: string) {
    try {
      await api.setActiveAccount(id);
      await refreshAccounts();
    } catch (e) {
      reportError("Could not switch account", e);
    }
  }

  async function remove(id: string) {
    try {
      await api.removeAccount(id);
      await refreshAccounts();
    } catch (e) {
      reportError("Could not remove that account", e);
    }
  }

  return (
    <Modal
      title="Accounts"
      subtitle="Sign in with Microsoft to play online, or add an offline account for single-player and LAN."
      onClose={onClose}
      width={520}
    >
      {accounts.length > 0 && (
        <div className={styles.list}>
          {accounts.map((account) => (
            <div
              key={account.id}
              className={clsx(styles.account, account.active && styles.active)}
            >
              <button className={styles.pick} onClick={() => select(account.id)}>
                <span
                  className={styles.avatar}
                  style={{
                    background: `linear-gradient(135deg, hsl(${hashHue(account.id)} 55% 42%), hsl(${(hashHue(account.id) + 40) % 360} 55% 30%))`,
                  }}
                >
                  {initials(account.username)}
                </span>
                <span className={styles.meta}>
                  <span className={styles.name}>{account.username}</span>
                  <span className={styles.kind}>
                    {account.kind === "microsoft" ? "Microsoft" : "Offline"}
                    {account.needsReauth && " · needs sign-in again"}
                  </span>
                </span>
                {account.active && <Check size={15} className={styles.check} />}
              </button>
              <button
                className="btn btn-ghost btn-icon btn-sm"
                onClick={() => remove(account.id)}
                title="Remove"
                aria-label={`Remove ${account.username}`}
              >
                <Trash2 size={14} />
              </button>
            </div>
          ))}
        </div>
      )}

      {pending ? (
        <div className={styles.deviceCode}>
          <p className={styles.deviceLead}>
            Enter this code at <strong>{pending.verificationUri}</strong>:
          </p>
          <div className={styles.codeRow}>
            <code className={styles.code}>{pending.userCode}</code>
            <button
              className="btn btn-sm"
              onClick={() => {
                void navigator.clipboard.writeText(pending.userCode);
                setCopied(true);
                setTimeout(() => setCopied(false), 1800);
              }}
            >
              {copied ? <Check size={13} /> : <Copy size={13} />}
              {copied ? "Copied" : "Copy"}
            </button>
            <button
              className="btn btn-sm"
              onClick={() => void openUrl(pending.verificationUri)}
            >
              <ExternalLink size={13} />
              Open
            </button>
          </div>
          <p className={styles.deviceWait}>
            <Loader2 size={13} className="spin" />
            Waiting for you to finish in the browser…
          </p>
        </div>
      ) : (
        <div>
          {config?.clientId ? (
            <button
              className="btn btn-primary"
              onClick={startMicrosoftLogin}
              disabled={signingIn}
              style={{ width: "100%" }}
            >
              {signingIn ? <Loader2 size={14} className="spin" /> : <UserPlus size={15} />}
              Sign in with Microsoft
            </button>
          ) : (
            <div className={styles.notice}>
              <p>
                Microsoft sign-in needs an Azure application ID. Microsoft issues
                these per app and Mojang approves each one individually, so Geode
                cannot ship with one.
              </p>
              <button
                className="btn btn-sm"
                onClick={() => {
                  navigate({ view: "settings", tab: "accounts" });
                  onClose();
                }}
              >
                Set it up in Settings
              </button>
            </div>
          )}
        </div>
      )}

      <hr className="divider" />

      <div>
        <label className="label" htmlFor="offline-name">
          Offline account
        </label>
        <div className={styles.offlineRow}>
          <input
            id="offline-name"
            className="input"
            placeholder="Username"
            value={offlineName}
            maxLength={16}
            onChange={(e) => setOfflineName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && addOffline()}
          />
          <button
            className="btn"
            onClick={addOffline}
            disabled={!offlineName.trim() || addingOffline}
          >
            Add
          </button>
        </div>
        <p className="hint">
          Offline accounts work for single-player and LAN. Servers with online
          mode enabled will reject them.
        </p>
      </div>
    </Modal>
  );
}
