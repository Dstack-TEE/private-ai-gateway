import React, { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { RotateCw } from "lucide-react";
import type { DesktopApi, UpdateInfo, UpdateChannel } from "../shared/contracts";
import { Button } from "./components/ui/button";
import { ToggleGroup, ToggleGroupItem } from "./components/ui/toggle-group";
import { FieldLabel } from "./components/ui/field";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions } from "./components/ui/item";
import { useErrorAlert } from "./lib/error-alert";

export function useUpdates(api: DesktopApi) {
  const [operation, setBusy] = useState<"restarting" | "changing">();
  const mounted = useRef(false);
  const inFlight = useRef(false);
  const readUpdate = useCallback(() => api.prepareUpdate(), [api]);
  const client = useQueryClient();
  const { data: snapshot, error: checkError, isFetching: checking, refetch } = useQuery<UpdateInfo>({ queryKey: ["app-update"], queryFn: readUpdate, enabled: !operation,
    refetchInterval: (query) => query.state.error ? 60_000 : 6 * 60 * 60_000, staleTime: 15 * 60_000, retry: false,
  });
  const { data: installedVersion } = useQuery({ queryKey: ["app-version"], queryFn: () => api.getAppVersion(), staleTime: Infinity });
  const info = checkError && snapshot ? { ...snapshot, version: null } : snapshot;
  const channel = info?.channel;
  const currentVersion = installedVersion ?? info?.currentVersion;
  const busy = operation ?? (checking ? "checking" : undefined);
  const error = checkError ? "Could not prepare software updates. Retrying automatically." : undefined;
  const reportError = useErrorAlert("Software update unavailable", undefined, api);
  const refresh = useCallback(async () => {
    await client.cancelQueries({ queryKey: ["app-update"] });
    return refetch();
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
      client.setQueryData<UpdateInfo | undefined>(["app-update"], (current) => current ? { ...current, channel: saved, version: null } : current);
      await refresh();
    } catch {
      if (mounted.current) reportError("Could not save update channel.");
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy(undefined);
    }
  };

  const restart = async () => {
    if (inFlight.current || checking || !info?.version) return;
    inFlight.current = true;
    setBusy("restarting");
    let retry = false;
    let installAttempted = false;
    try {
      const latest = await refresh();
      if (latest.error) throw latest.error;
      if (!latest.data?.version) return;
      if (!await api.confirm({ title: "Restart to update?", message: `Version ${latest.data.version} is ready. Protection will stop and connected agent configurations will be restored before the app restarts. In-flight requests may be interrupted.`, confirmLabel: "Restart to update" })) return;
      installAttempted = true;
      await api.restartToUpdate();
    } catch (failure) {
      if (mounted.current) {
        reportError(failure);
        retry = installAttempted;
      }
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy(undefined);
    }
    if (retry && mounted.current) void refresh();
  };
  return { info, currentVersion, busy, error, channel, changeChannel, restart };
}

export function UpdateChannelControl({ updates }: { updates: ReturnType<typeof useUpdates> }): React.JSX.Element {
  const systemManaged = updates.info?.systemManaged === true;
  return <Item>
    <ItemContent>
      <ItemTitle><FieldLabel id="update-channel-label">Update channel</FieldLabel></ItemTitle>
      <ItemDescription id="update-channel-note">{systemManaged ? "Managed by pacman" : updates.channel === "beta" ? "Pre-release builds" : "Stable releases"}</ItemDescription>
    </ItemContent>
    <ItemActions>
      <ToggleGroup size="sm" variant="outline" spacing={0} aria-labelledby="update-channel-label" aria-describedby="update-channel-note" value={updates.channel ? [updates.channel] : []} disabled={systemManaged || !updates.channel || Boolean(updates.busy)} onValueChange={([value]) => {
        if (value === "stable" || value === "beta") void updates.changeChannel(value);
      }}><ToggleGroupItem value="stable">Stable</ToggleGroupItem><ToggleGroupItem value="beta">Beta</ToggleGroupItem></ToggleGroup>
    </ItemActions>
  </Item>;
}

export function UpdateControl({ updates, productName }: { updates: ReturnType<typeof useUpdates>; productName: string }): React.JSX.Element {
  const { info, currentVersion, busy, error } = updates;
  const label = busy === "changing" ? "Saving update channel…" : busy === "restarting" ? "Restarting to update…" : busy === "checking" ? "Checking for updates…"
    : error ? "Update status unavailable" : (info?.systemManaged ? "Updates are managed by pacman"
      : info?.enabled === false ? "Automatic updates unavailable in this build"
      : info?.channelPublished === false ? "No releases published in this channel yet"
      : info?.version ? `Version ${info.version} is available`
      : info ? "You're up to date" : "Update status unavailable");
  return <Item>
    <ItemContent>
      <ItemTitle>{productName}</ItemTitle>
    </ItemContent>
    <ItemActions className="ml-auto max-w-full flex-wrap justify-end text-right">
      <span className="text-sm font-medium tabular-nums" data-slot="app-version">{currentVersion ? `v${currentVersion}` : "Version unavailable"}</span>
      {info?.version ? <Button disabled={Boolean(busy)} onClick={() => void updates.restart()}><RotateCw aria-hidden="true" />Restart to update</Button> : <ItemDescription className="max-w-sm text-right" role="status">{label}</ItemDescription>}
      {info?.version && <span role="status" className="sr-only">{label}</span>}
    </ItemActions>
  </Item>;
}
