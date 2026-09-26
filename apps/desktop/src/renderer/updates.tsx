import React, { useCallback, useMemo, useRef } from "react";
import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { RotateCw } from "lucide-react";
import type { DesktopApi, UpdateInfo, UpdateChannel } from "../shared/contracts";
import { Button } from "./components/ui/button";
import { ToggleGroup, ToggleGroupItem } from "./components/ui/toggle-group";
import { FieldLabel } from "./components/ui/field";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions } from "./components/ui/item";
import { useConfirm } from "./components/confirm";
import { toastError } from "./lib/error-message";

const CHECK_INTERVAL = 6 * 60 * 60_000;

/** Channel changes and installs, which run one at a time and pause checks. */
const UPDATE_OPERATION = ["app-update-operation"];

/** `checks` is false only where the App Store owns updates. */
export function useUpdates(api: DesktopApi, checks: boolean) {
  const client = useQueryClient();
  const confirm = useConfirm();
  const operating = useIsMutating({ mutationKey: UPDATE_OPERATION }) > 0;
  // The query counts every failed check; these came before the last success.
  const earlierFailures = useRef(0);
  const { data: snapshot, error: checkError, isFetching: checking, refetch } = useQuery<UpdateInfo>({
    queryKey: ["app-update"], queryFn: () => api.prepareUpdate(), enabled: checks && !operating,
    // A failed check runs again after 1 minute, then backs off up to the
    // interval. Unlike retries, which wait for the window to be focused, the
    // interval keeps running while it is hidden: a tray app prepares updates
    // while its window is hidden or inactive.
    refetchInterval: ({ state }) => {
      if (state.status === "success") earlierFailures.current = state.errorUpdateCount;
      const failures = state.errorUpdateCount - earlierFailures.current;
      return failures ? Math.min(60_000 * 2 ** (failures - 1), CHECK_INTERVAL) : CHECK_INTERVAL;
    },
    refetchIntervalInBackground: true,
    staleTime: 15 * 60_000,
    retry: false,
  });
  const { data: installedVersion } = useQuery({ queryKey: ["app-version"], queryFn: () => api.getAppVersion(), staleTime: Infinity });
  /** Checks again now, in place of a check in progress. */
  const recheck = useCallback(async () => {
    await client.cancelQueries({ queryKey: ["app-update"] });
    void refetch();
  }, [client, refetch]);
  const scope = { id: "app-update" };
  const changeChannel = useMutation({
    mutationKey: UPDATE_OPERATION,
    scope,
    mutationFn: (next: UpdateChannel) => api.setUpdateChannel(next),
    onSuccess: async (saved) => {
      client.setQueryData<UpdateInfo | undefined>(["app-update"], (current) => current ? { ...current, channel: saved, version: null } : current);
      await recheck();
    },
    onError: (error) => toastError("Could not change the update channel", error),
  });
  const restart = useMutation({
    mutationKey: UPDATE_OPERATION,
    scope,
    mutationFn: async () => {
      // The latest release, checked once: a failure ends the restart.
      await client.cancelQueries({ queryKey: ["app-update"] });
      const latest = await api.prepareUpdate();
      client.setQueryData(["app-update"], latest);
      if (!latest.version) return;
      if (!await confirm({ title: "Restart to update?", message: `Version ${latest.version} is ready. Protection will pause during the restart and resume only after fresh verification. In-flight requests may be interrupted.`, confirmLabel: "Restart to Update" })) return;
      try {
        await api.restartToUpdate();
      } catch (error) {
        // A failed install used the prepared update; prepare it again.
        void recheck();
        throw error;
      }
    },
    onError: (error) => toastError("Could not install the update", error),
  });

  return useMemo(() => {
    const info = checkError && snapshot ? { ...snapshot, version: null } : snapshot;
    const channel = info?.channel;
    // Only in-app installs restart to update; other installations show their upgrade steps.
    const ready = Boolean(info?.enabled && info.version);
    const currentVersion = installedVersion ?? info?.currentVersion;
    const busy = changeChannel.isPending ? "changing" : restart.isPending ? "restarting" : checking ? "checking" : undefined;
    const error = checkError ? "Could not prepare software updates. Retrying automatically." : undefined;
    return {
      info, ready, currentVersion, busy, error, channel, checks,
      changeChannel: (next: UpdateChannel) => {
        if (!busy && next !== channel) changeChannel.mutate(next);
      },
      restart: () => {
        if (!busy && ready) restart.mutate();
      },
    };
  }, [checks, snapshot, checkError, checking, installedVersion, changeChannel.isPending, changeChannel.mutate, restart.isPending, restart.mutate]);
}

export function UpdateChannelControl({ updates }: { updates: ReturnType<typeof useUpdates> }): React.JSX.Element {
  return <Item>
    <ItemContent>
      <ItemTitle><FieldLabel id="update-channel-label">Update channel</FieldLabel></ItemTitle>
      <ItemDescription id="update-channel-note">{updates.channel === "beta" ? "Beta and stable releases" : "Stable releases"}</ItemDescription>
    </ItemContent>
    <ItemActions>
      <ToggleGroup size="sm" variant="outline" spacing={0} aria-labelledby="update-channel-label" aria-describedby="update-channel-note" value={updates.channel ? [updates.channel] : []} disabled={!updates.channel || Boolean(updates.busy)} onValueChange={([value]) => {
        if (value === "stable" || value === "beta") updates.changeChannel(value);
      }}><ToggleGroupItem value="stable">Stable</ToggleGroupItem><ToggleGroupItem value="beta">Beta</ToggleGroupItem></ToggleGroup>
    </ItemActions>
  </Item>;
}

export function UpdateControl({ updates, productName, desktop }: { updates: ReturnType<typeof useUpdates>; productName: string; desktop: boolean }): React.JSX.Element {
  const { info, ready, currentVersion, busy, error } = updates;
  const label = busy === "changing" ? "Saving update channel…" : busy === "restarting" ? "Restarting to update…" : busy === "checking" ? "Checking for updates…"
    : !updates.checks ? "Updates are provided by the App Store"
    : error || !info ? "Update status unavailable"
    : !info.enabled && !info.systemManaged ? "Automatic updates unavailable in this build"
    : info.version ? `Version ${info.version} is available`
    : "You’re up to date";
  const manual = !busy && !ready && info?.version ? info : undefined;
  const commands = manual?.upgradeCommands ?? [];
  return <Item>
    <ItemContent className="min-w-0">
      <ItemTitle>{productName}</ItemTitle>
      {manual && <>
        <ItemDescription>{commands.length ? (desktop ? `Quit ${productName}, then run:` : "Run:")
          : manual.downloadUrl ? "Extract this archive into a new directory:" : `Update it from the ${productName} desktop app or its package manager.`}</ItemDescription>
        {(commands.length > 0 || manual.downloadUrl) && <pre className="whitespace-pre-wrap break-all rounded-md bg-muted px-3 py-2 text-xs select-text" data-slot="upgrade-commands">{commands.length ? commands.join("\n") : manual.downloadUrl}</pre>}
      </>}
    </ItemContent>
    <ItemActions className="ml-auto max-w-full flex-wrap justify-end text-right">
      <span className="text-sm font-medium tabular-nums" data-slot="app-version">{currentVersion ? `v${currentVersion}` : "Version unavailable"}</span>
      {ready ? <Button disabled={Boolean(busy)} onClick={updates.restart}><RotateCw aria-hidden="true" />Restart to Update</Button> : <ItemDescription className="max-w-sm text-right" role="status">{label}</ItemDescription>}
      {ready && <span role="status" className="sr-only">{label}</span>}
    </ItemActions>
  </Item>;
}
