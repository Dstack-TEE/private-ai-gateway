import React, { useCallback, useEffect, useRef, useState } from "react";
import { Download } from "lucide-react";
import type { DesktopApi, UpdateInfo, UpdateProgress, UpdateChannel } from "../shared/contracts";
import { Button } from "./components/ui/button";
import { NativeSelect } from "./components/ui/native-select";
import { FieldLabel } from "./components/ui/field";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions } from "./components/ui/item";

export function useUpdates(api: DesktopApi) {
  const [info, setInfo] = useState<UpdateInfo>();
  const [currentVersion, setCurrentVersion] = useState<string>();
  const [busy, setBusy] = useState<"checking" | "installing" | "changing">();
  const [channel, setChannel] = useState<UpdateChannel>();
  const [error, setError] = useState<string>();
  const [progress, setProgress] = useState<UpdateProgress>();
  const mounted = useRef(false);
  const inFlight = useRef(false);
  const lastCheck = useRef(0);

  const refresh = useCallback(async () => {
    setError(undefined);
    setInfo((current) => current ? { ...current, version: null } : current);
    try {
      const selected = await api.getUpdateChannel();
      if (mounted.current) setChannel(selected);
      const next = await api.checkUpdate();
      if (mounted.current) { setInfo(next); setCurrentVersion(next.currentVersion); }
    } catch {
      if (mounted.current) setError("Could not check for updates. Retrying automatically.");
    } finally {
      lastCheck.current = Date.now();
    }
  }, [api]);

  const check = useCallback(async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy("checking");
    try { await refresh(); }
    finally { inFlight.current = false; if (mounted.current) setBusy(undefined); }
  }, [refresh]);

  useEffect(() => {
    mounted.current = true;
    const unsubscribe = api.onUpdateProgress(setProgress);
    void api.getAppVersion().then((version) => { if (mounted.current) setCurrentVersion(version); }).catch(() => {
      // A successful update check can still supply the installed version.
    });
    void check();
    const online = () => { void check(); };
    const focus = () => { if (Date.now() - lastCheck.current >= 15 * 60_000) void check(); };
    const timer = window.setInterval(() => { void check(); }, 6 * 60 * 60_000);
    window.addEventListener("online", online);
    window.addEventListener("focus", focus);
    return () => {
      mounted.current = false;
      unsubscribe();
      window.clearInterval(timer);
      window.removeEventListener("online", online);
      window.removeEventListener("focus", focus);
    };
  }, [api, check]);

  const changeChannel = async (next: UpdateChannel) => {
    if (inFlight.current || next === channel) return;
    inFlight.current = true;
    setBusy("changing");
    setError(undefined);
    try {
      const saved = await api.setUpdateChannel(next);
      if (!mounted.current) return;
      setChannel(saved);
      await refresh();
    } catch {
      if (mounted.current) setError("Could not save update channel.");
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy(undefined);
    }
  };

  const install = async () => {
    if (inFlight.current || !info?.version) return;
    inFlight.current = true;
    setError(undefined);
    setBusy("installing");
    try {
      if (!await api.confirm({ title: "Install update?", message: "Protection will stop and connected agent configurations will be restored before the app restarts. In-flight requests may be interrupted.", confirmLabel: "Install and Restart" })) return;
      setProgress(undefined);
      await api.installUpdate();
    } catch {
      if (mounted.current) {
        setError("Update installation failed. A new check will run automatically.");
        setInfo((current) => current ? { ...current, version: null } : current);
      }
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy(undefined);
    }
  };
  return { info, currentVersion, busy, error, progress, channel, changeChannel, install };
}

export function UpdateChannelControl({ updates }: { updates: ReturnType<typeof useUpdates> }): React.JSX.Element {
  return <Item>
    <ItemContent>
      <ItemTitle><FieldLabel htmlFor="update-channel">Update channel</FieldLabel></ItemTitle>
      <ItemDescription id="update-channel-note">{updates.channel === "beta" ? "Pre-release builds" : "Stable releases"}</ItemDescription>
    </ItemContent>
    <ItemActions>
      <NativeSelect className="w-36" id="update-channel" aria-describedby="update-channel-note" value={updates.channel ?? ""} disabled={!updates.channel || Boolean(updates.busy)} onChange={(event) => {
        const value = event.target.value;
        if (value === "stable" || value === "beta") void updates.changeChannel(value);
      }}>
        {!updates.channel && <option value="" disabled>Loading</option>}
        <option value="stable">Stable</option>
        <option value="beta">Beta</option>
      </NativeSelect>
    </ItemActions>
  </Item>;
}

export function UpdateControl({ updates, productName }: { updates: ReturnType<typeof useUpdates>; productName: string }): React.JSX.Element {
  const { info, currentVersion, busy, error, progress } = updates;
  const label = busy === "changing" ? "Saving update channel…" : busy === "checking" ? "Checking for updates…"
    : busy === "installing" ? progress?.total ? `Downloading ${Math.min(100, Math.floor(progress.downloaded / progress.total * 100))}%` : "Preparing update…"
    : error ?? (info?.enabled === false ? "Automatic updates unavailable in this build"
      : info?.channelPublished === false ? "No releases published in this channel yet"
      : info?.version ? `Version ${info.version} is available`
      : info ? "You're up to date" : "Update status unavailable");
  return <Item>
    <ItemContent>
      <ItemTitle>{productName}<span className="font-normal text-muted-foreground" data-slot="app-version">{currentVersion ? `v${currentVersion}` : "Version unavailable"}</span></ItemTitle>
      <ItemDescription role="status">{label}</ItemDescription>
    </ItemContent>
    {info?.version && <ItemActions><Button disabled={Boolean(busy)} onClick={() => void updates.install()}><Download aria-hidden="true" />Install and Restart</Button></ItemActions>}
  </Item>;
}
