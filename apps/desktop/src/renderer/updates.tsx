import React, { useCallback, useEffect, useRef, useState } from "react";
import { Download, RefreshCw } from "lucide-react";
import type { DesktopApi, UpdateInfo, UpdateProgress } from "../shared/contracts";

export function useUpdates(api: DesktopApi) {
  const [info, setInfo] = useState<UpdateInfo>();
  const [busy, setBusy] = useState<"checking" | "installing">();
  const [error, setError] = useState<string>();
  const [progress, setProgress] = useState<UpdateProgress>();
  const mounted = useRef(false);
  const check = useCallback(async () => {
    setBusy("checking");
    setError(undefined);
    setInfo((current) => current ? { ...current, version: null } : current);
    try {
      const next = await api.checkUpdate();
      if (mounted.current) setInfo(next);
    } catch {
      if (mounted.current) setError("Could not check for updates. Try again later.");
    } finally {
      if (mounted.current) setBusy(undefined);
    }
  }, [api]);
  useEffect(() => {
    mounted.current = true;
    const unsubscribe = api.onUpdateProgress(setProgress);
    void check();
    return () => { mounted.current = false; unsubscribe(); };
  }, [api, check]);
  const install = async () => {
    setError(undefined);
    setBusy("installing");
    try {
      if (!await api.confirm({ title: "Install update?", message: "Protection will stop and connected agent configurations will be restored before the app restarts. In-flight requests may be interrupted.", confirmLabel: "Install and Restart" })) return;
      setProgress(undefined);
      await api.installUpdate();
    } catch (cause) {
      if (mounted.current) {
        setError(cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "Could not install the update. Check for updates to retry.");
        setInfo((current) => current ? { ...current, version: null } : current);
      }
    } finally {
      if (mounted.current) setBusy(undefined);
    }
  };
  return { info, busy, error, progress, check, install };
}

export function UpdateSettings({ updates }: { updates: ReturnType<typeof useUpdates> }): React.JSX.Element {
  const { info, busy, error, progress } = updates;
  const label = busy === "checking" ? "Checking for updates…"
    : busy === "installing" ? progress?.total ? `Downloading ${Math.min(100, Math.floor(progress.downloaded / progress.total * 100))}%` : "Preparing update…"
    : error ? "Update could not complete"
    : info?.enabled === false ? "Updates are not configured for this build"
    : info?.version ? `Version ${info.version} is available`
    : info ? "You're up to date" : "Update status unavailable";
  return <section className="group" aria-labelledby="updates-title">
    <h2 className="group-title" id="updates-title">Updates</h2>
    <div className="inset"><div className="row">
      <span className="row-main"><span className="row-title" role="status">{label}</span><span className="row-note">{info ? `Installed version ${info.currentVersion}` : ""}</span></span>
      {info?.version
        ? <button className="button" disabled={Boolean(busy)} onClick={() => void updates.install()}><Download size={14} />Install and Restart…</button>
        : <button className="button" disabled={Boolean(busy) || info?.enabled === false} onClick={() => void updates.check()}><RefreshCw size={14} />Check for Updates</button>}
    </div></div>
    {error && <p className="banner" role="alert">{error}</p>}
  </section>;
}
