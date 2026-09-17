import React, { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Download } from "lucide-react";
import type { DesktopApi, UpdateInfo, UpdateProgress, UpdateChannel } from "../shared/contracts";
import { Button } from "./components/ui/button";
import { ToggleGroup, ToggleGroupItem } from "./components/ui/toggle-group";
import { FieldLabel } from "./components/ui/field";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions } from "./components/ui/item";
import { Progress } from "./components/ui/progress";
import { useErrorAlert } from "./lib/error-alert";

export function useUpdates(api: DesktopApi) {
  const [operation, setBusy] = useState<"installing" | "changing">();
  const mounted = useRef(false);
  const inFlight = useRef(false);
  const readUpdate = useCallback(async () => {
    const channel = await api.getUpdateChannel();
    return { channel, info: await api.checkUpdate() };
  }, [api]);
  const client = useQueryClient();
  const { data: snapshot, error: checkError, isFetching: checking, refetch } = useQuery<{
    channel: UpdateChannel; info?: UpdateInfo;
  }>({ queryKey: ["app-update"], queryFn: readUpdate, enabled: !operation,
    refetchInterval: (query) => query.state.error ? 60_000 : 6 * 60 * 60_000, staleTime: 15 * 60_000, retry: false,
  });
  const { data: installedVersion } = useQuery({ queryKey: ["app-version"], queryFn: () => api.getAppVersion(), staleTime: Infinity });
  const info = checkError && snapshot?.info ? { ...snapshot.info, version: null } : snapshot?.info;
  const channel = snapshot?.channel;
  const currentVersion = installedVersion ?? info?.currentVersion;
  const busy = operation ?? (checking ? "checking" : undefined);
  const error = checkError ? "Could not check for updates. Retrying automatically." : undefined;
  const reportError = useErrorAlert("Software update unavailable", undefined, api);
  const refresh = useCallback(async () => {
    await client.cancelQueries({ queryKey: ["app-update"] });
    await refetch();
  }, [client, refetch]);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, [api]);

  const changeChannel = async (next: UpdateChannel) => {
    if (inFlight.current || checking || next === channel) return;
    inFlight.current = true;
    setBusy("changing");
    try {
      const saved = await api.setUpdateChannel(next);
      if (!mounted.current) return;
      client.setQueryData(["app-update"], { channel: saved });
      await refresh();
    } catch {
      if (mounted.current) reportError("Could not save update channel.");
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy(undefined);
    }
  };

  const install = async () => {
    if (inFlight.current || checking || !info?.version) return;
    inFlight.current = true;
    setBusy("installing");
    let dialogOpened = false;
    try {
      if (!await api.confirm({ title: "Install update?", message: "Protection will stop and connected agent configurations will be restored before the app restarts. In-flight requests may be interrupted.", confirmLabel: "Install and Restart" })) return;
      await api.openNativeDialog("update-progress");
      dialogOpened = true;
      await api.installUpdate();
    } catch {
      if (mounted.current) {
        if (!dialogOpened) {
          reportError("Could not open the update dialog. Please try again.");
        } else {
          // Installing consumes the native update handle. Refresh it for a retry
          // without duplicating the install error outside its progress window.
          await refresh();
        }
      }
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy(undefined);
    }
  };
  return { info, currentVersion, busy, error, channel, changeChannel, install };
}

export function UpdateProgressMeter({ progress }: { progress?: UpdateProgress }): React.JSX.Element {
  const percent = progress?.total ? Math.min(100, Math.floor(progress.downloaded / progress.total * 100)) : null;
  return <><Progress value={percent} aria-label="Update progress" /><p className="text-sm text-muted-foreground" role="status">{percent === 100 ? "Verifying and installing…" : percent === null ? "Preparing download…" : `Downloading ${percent}%`}</p></>;
}

export function UpdateChannelControl({ updates }: { updates: ReturnType<typeof useUpdates> }): React.JSX.Element {
  return <Item>
    <ItemContent>
      <ItemTitle><FieldLabel id="update-channel-label">Update channel</FieldLabel></ItemTitle>
      <ItemDescription id="update-channel-note">{updates.channel === "beta" ? "Pre-release builds" : "Stable releases"}</ItemDescription>
    </ItemContent>
    <ItemActions>
      <ToggleGroup size="sm" variant="outline" spacing={0} aria-labelledby="update-channel-label" aria-describedby="update-channel-note" value={updates.channel ? [updates.channel] : []} disabled={!updates.channel || Boolean(updates.busy)} onValueChange={([value]) => {
        if (value === "stable" || value === "beta") void updates.changeChannel(value);
      }}><ToggleGroupItem value="stable">Stable</ToggleGroupItem><ToggleGroupItem value="beta">Beta</ToggleGroupItem></ToggleGroup>
    </ItemActions>
  </Item>;
}

export function UpdateControl({ updates, productName }: { updates: ReturnType<typeof useUpdates>; productName: string }): React.JSX.Element {
  const { info, currentVersion, busy, error } = updates;
  const label = busy === "changing" ? "Saving update channel…" : busy === "checking" ? "Checking for updates…"
    : error ? "Update status unavailable" : (info?.enabled === false ? "Automatic updates unavailable in this build"
      : info?.channelPublished === false ? "No releases published in this channel yet"
      : info?.version ? `Version ${info.version} is available`
      : info ? "You're up to date" : "Update status unavailable");
  return <Item>
    <ItemContent>
      <ItemTitle>{productName}</ItemTitle>
    </ItemContent>
    <ItemActions className="ml-auto max-w-full flex-wrap justify-end text-right">
      <span className="text-sm font-medium tabular-nums" data-slot="app-version">{currentVersion ? `v${currentVersion}` : "Version unavailable"}</span>
      {info?.version ? <Button disabled={Boolean(busy)} onClick={() => void updates.install()}><Download aria-hidden="true" />Install and Restart</Button> : <ItemDescription className="max-w-sm text-right" role="status">{label}</ItemDescription>}
      {info?.version && <span role="status" className="sr-only">{label}</span>}
    </ItemActions>
  </Item>;
}
