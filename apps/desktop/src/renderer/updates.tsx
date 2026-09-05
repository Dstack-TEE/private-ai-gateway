import { Button } from "./components/ui/button";
import React, { useCallback, useEffect, useRef, useState } from "react";
import { Download, RefreshCw } from "lucide-react";
import type { DesktopApi, UpdateInfo, UpdateProgress, UpdateChannel } from "../shared/contracts";
import { NativeSelect } from "./components/ui/native-select";
import { Field, FieldLabel, FieldDescription } from "./components/ui/field";

export function useUpdates(api: DesktopApi) {
  const [info, setInfo] = useState<UpdateInfo>();
  const [busy, setBusy] = useState<"checking" | "installing" | "changing">();
  const [channel, setChannel] = useState<UpdateChannel>();
  const [error, setError] = useState<string>();
  const [progress, setProgress] = useState<UpdateProgress>();
  const mounted = useRef(false);
  const check = useCallback(async () => {
    setBusy("checking");
    setError(undefined);
    setInfo((current) => current ? { ...current, version: null } : current);
    try {
      const selected = await api.getUpdateChannel();
      if (mounted.current) setChannel(selected);
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
  const changeChannel = async (next: UpdateChannel) => {
    if (busy || next === channel) return;
    setBusy("changing");
    setError(undefined);
    try {
      const saved = await api.setUpdateChannel(next);
      if (!mounted.current) return;
      setChannel(saved);
      await check();
    } catch {
      if (mounted.current) setError("Could not save update channel. Try again.");
    } finally {
      if (mounted.current) setBusy(undefined);
    }
  };
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
  return { info, busy, error, progress, channel, changeChannel, check, install };
}

export function UpdateChannelControl({ updates }: { updates: ReturnType<typeof useUpdates> }): React.JSX.Element {
  return <Field>
    <FieldLabel htmlFor="update-channel">Update channel</FieldLabel>
    <NativeSelect id="update-channel" value={updates.channel ?? ""} disabled={!updates.channel || Boolean(updates.busy)} onChange={(event) => {
      const value = event.target.value;
      if (value === "stable" || value === "beta") void updates.changeChannel(value);
    }}>
      {!updates.channel && <option value="" disabled>Loading</option>}
      <option value="stable">Stable</option>
      <option value="beta">Beta</option>
    </NativeSelect>
    <FieldDescription>{updates.channel === "beta" ? "Pre-release updates. Switching to Stable never downgrades this installation." : "Stable releases only."}</FieldDescription>
  </Field>;
}

export function UpdateControl({ updates }: { updates: ReturnType<typeof useUpdates> }): React.JSX.Element {
  const { info, busy, error, progress } = updates;
  const label = busy === "changing" ? "Saving update channel…" : busy === "checking" ? "Checking for updates…"
    : busy === "installing" ? progress?.total ? `Downloading ${Math.min(100, Math.floor(progress.downloaded / progress.total * 100))}%` : "Preparing update…"
    : error ? "Update could not complete"
    : info?.enabled === false ? "Updates are not configured for this build"
    : info?.version ? `Version ${info.version} is available`
    : info ? "You're up to date" : "Update status unavailable";
  const action = info?.version ? "Install and Restart…" : "Check for Updates";
  return <span className="update-control">
    <Button variant="outline" aria-label={action} title={action} disabled={Boolean(busy) || info?.enabled === false} onClick={() => void (info?.version ? updates.install() : updates.check())}>
      {info?.currentVersion ? `v${info.currentVersion}` : "Updates"}
      {info?.version ? <Download size={14} aria-hidden="true" /> : <RefreshCw size={14} className={busy ? "is-spinning" : undefined} aria-hidden="true" />}
    </Button>
    <small role="status">{label}</small>
    {error && <small role="alert">{error}</small>}
  </span>;
}
