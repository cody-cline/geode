import { useCallback, useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import {
  Check,
  Copy,
  DownloadCloud,
  ExternalLink,
  Loader2,
  RefreshCw,
  Trash2,
  UserPlus,
} from "lucide-react";

import Modal from "./Modal";
import { api } from "../lib/api";
import { hashHue, initials } from "../lib/format";
import { useStore } from "../lib/store";
import type { DeviceCodeStart, ImportCandidate, ImportScan } from "../lib/types";

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
  const [scan, setScan] = useState<ImportScan | null>(null);
  const [scanning, setScanning] = useState(true);
  const [importing, setImporting] = useState<string | null>(null);

  // Scan on open. It is a handful of file reads plus one API call per token,
  // and finding an existing session is the answer for most people.
  const rescan = useCallback(async () => {
    setScanning(true);
    try {
      setScan(await api.findImportableAccounts());
    } catch {
      setScan({ candidates: [], notes: [] });
    } finally {
      setScanning(false);
    }
  }, []);

  useEffect(() => {
    void rescan();
  }, [rescan]);

  const known = new Set(accounts.map((a) => a.id));
  const importable = (scan?.candidates ?? []).filter((c) => !known.has(c.uuid));
  const notes = scan?.notes ?? [];

  async function importSession(candidate: ImportCandidate) {
    setImporting(candidate.uuid);
    try {
      const account = await api.importAccount(candidate.uuid);
      await refreshAccounts();
      toast({
        kind: "success",
        title: `Imported ${account.username}`,
        body: `Using the session from ${candidate.sourceLabel}.`,
      });
    } catch (e) {
      reportError("Could not import that session", e);
    } finally {
      setImporting(null);
    }
  }

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
                    {account.kind === "microsoft"
                      ? "Microsoft"
                      : account.kind === "imported"
                        ? `via ${account.sourceLabel ?? "another launcher"}`
                        : "Offline"}
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
        <div className={styles.section}>
          {/* Importing needs no Azure application at all, so it leads. */}
          {scanning ? (
            <p className={styles.scanning}>
              <Loader2 size={13} className="spin" />
              Looking for sessions in other launchers…
            </p>
          ) : importable.length > 0 ? (
            <div className={styles.importBlock}>
              <p className={styles.importLead}>
                <strong>Found an existing session.</strong> Use your own
                Minecraft login from another launcher on this machine — no Azure
                setup needed.
              </p>
              {importable.map((candidate) => (
                <div key={candidate.uuid} className={styles.importRow}>
                  <span
                    className={styles.avatar}
                    style={{
                      background: `linear-gradient(135deg, hsl(${hashHue(candidate.uuid)} 55% 42%), hsl(${(hashHue(candidate.uuid) + 40) % 360} 55% 30%))`,
                    }}
                  >
                    {initials(candidate.username)}
                  </span>
                  <span className={styles.meta}>
                    <span className={styles.name}>{candidate.username}</span>
                    <span className={styles.kind}>
                      from {candidate.sourceLabel}
                    </span>
                  </span>
                  <button
                    className="btn btn-sm btn-primary"
                    onClick={() => importSession(candidate)}
                    disabled={importing !== null}
                  >
                    {importing === candidate.uuid ? (
                      <Loader2 size={12} className="spin" />
                    ) : (
                      <DownloadCloud size={13} />
                    )}
                    Use this
                  </button>
                </div>
              ))}
              <p className={styles.importNote}>
                Geode borrows only the session token, never the other launcher's
                app credentials — so it can't renew it. When it lapses, open
                that launcher once and Geode picks the fresh one up by itself.
              </p>
            </div>
          ) : null}

          {config?.hasClientId ? (
            <button
              className="btn btn-primary"
              onClick={startMicrosoftLogin}
              disabled={signingIn}
              style={{ width: "100%" }}
            >
              {signingIn ? <Loader2 size={14} className="spin" /> : <UserPlus size={15} />}
              Sign in with Microsoft
            </button>
          ) : importable.length > 0 ? (
            // An import is available, so the Azure route is a footnote rather
            // than a wall of setup instructions.
            <details className={styles.altAuth}>
              <summary>Or sign in directly with Microsoft</summary>
              <p>
                Needs your own Azure application ID, which Mojang has to approve
                for Minecraft API access. Only worth it if you want Geode to
                stand on its own without another launcher installed.
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
            </details>
          ) : (
            <div className={styles.notice}>
              {notes.length > 0 ? (
                notes.map((note) => (
                  <p key={note} className={styles.noteLine}>
                    {note}
                  </p>
                ))
              ) : (
                <p>No other launcher with a signed-in account was found here.</p>
              )}
              <p className={styles.noticeAlt}>
                Signing in directly instead needs your own Azure application ID —
                Microsoft issues them per app and Mojang approves each one, so
                Geode cannot ship one.
              </p>
              <div className={styles.noticeActions}>
                <button className="btn btn-sm" onClick={rescan} disabled={scanning}>
                  {scanning ? (
                    <Loader2 size={13} className="spin" />
                  ) : (
                    <RefreshCw size={13} />
                  )}
                  Scan again
                </button>
                <button
                  className="btn btn-sm"
                  onClick={() => {
                    navigate({ view: "settings", tab: "accounts" });
                    onClose();
                  }}
                >
                  Set up Microsoft sign-in
                </button>
              </div>
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
