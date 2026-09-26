import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { DesktopApi, NotificationConfiguration, NotificationPreferences } from "../../shared/contracts";
import { SettingsList, SettingsToggle } from "./settings";
import { AppDialog, type DialogControl } from "./app-dialog";
import { Alert, AlertDescription } from "./ui/alert";
import { DialogFooter } from "./ui/dialog";
import { Button } from "./ui/button";

/**
 * The notification preferences and the system permission. The permission is
 * requested only when the user turns notifications on or asks for it, as
 * Apple's guidance recommends: in context, when the app needs it.
 */
function useNotificationSettings(api: DesktopApi) {
  const client = useQueryClient();
  const { data, error: readError } = useQuery({
    queryKey: ["notifications"], queryFn: () => api.getNotificationSettings(), staleTime: 0,
  });
  const refresh = () => client.invalidateQueries({ queryKey: ["notifications"] });
  // Resolves whether the system permission the change needs could be requested.
  const change = useMutation({
    mutationFn: async ({ key, enabled, current }: { key: keyof NotificationPreferences; enabled: boolean; current: NotificationConfiguration }) => {
      await api.saveNotificationSettings({ ...current.preferences, [key]: enabled });
      if (key !== "enabled" || !enabled || current.permission !== "notDetermined") return true;
      return api.requestNotificationPermission().then(() => true, () => false);
    },
    onSettled: refresh,
  });
  const permission = useMutation({
    mutationFn: async () => {
      if (data?.permission === "notDetermined") await api.requestNotificationPermission();
      else await api.openNotificationSettings();
    },
    onSettled: refresh,
  });
  const busy = change.isPending || permission.isPending;
  const error = change.error ? "Could not save notification settings."
    : change.data === false ? "Notifications are enabled in this app, but system permission could not be requested."
    : permission.error ? "Could not open notification permissions. Check your system settings."
    : readError ? "Could not read notification settings." : undefined;
  return {
    data,
    error,
    busy,
    // One result shows at a time: the latest action's.
    change: (key: keyof NotificationPreferences, enabled: boolean) => {
      if (!data) return;
      permission.reset();
      change.mutate({ key, enabled, current: data });
    },
    permissionAction: () => {
      change.reset();
      permission.mutate();
    },
  };
}

function NotificationPermissionNotice({ data, busy, permissionAction }: Pick<ReturnType<typeof useNotificationSettings>, "data" | "busy" | "permissionAction">) {
  if (!data || !data.preferences.enabled || (data.permission === "granted" && data.alertsEnabled !== false)) return null;
  const supported = data.permission !== "unsupported";
  return <Alert className="border-warning/30 bg-warning/10">
    <AlertDescription className="flex flex-wrap items-center justify-between gap-3 text-warning">
      <span>{data.permission === "granted" ? "Notifications are allowed, but banner alerts are disabled in system settings." : data.permission === "denied" ? "Notifications are disabled in system settings." : data.permission === "notDetermined" ? "System permission is needed to show notifications." : "System notification permission could not be confirmed. Check your desktop notification settings."}</span>
      {supported && <Button variant="outline" size="sm" disabled={busy} onClick={permissionAction}>{data.permission === "notDetermined" ? "Allow Notifications" : "System Settings"}</Button>}
    </AlertDescription>
  </Alert>;
}

export function NotificationsDialog({ api, ...control }: { api: DesktopApi } & DialogControl) {
  const notifications = useNotificationSettings(api);
  const { data, error, busy, change } = notifications;
  return <AppDialog {...control} title="Notifications" className="sm:max-w-xl" dismissible={!busy}>
    <div className="-mx-6 flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-6 py-1">
      <NotificationPermissionNotice {...notifications} />
      {error && <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert>}
      {data && <>
        <SettingsList><SettingsToggle label="Allow notifications" checked={data.preferences.enabled} disabled={busy} onToggle={() => change("enabled", !data.preferences.enabled)} /></SettingsList>
        <SettingsList>{([
          ["gateway", "Protection problems", "Protection, connection and agent configuration errors."],
          ["localApi", "Local API problems", "The local listener becomes unavailable."],
          ["verification", "Response verification failures", "A response fails proof verification."],
        ] as const).map(([key, label, description]) => <SettingsToggle key={key} label={label} description={description} checked={data.preferences[key]} disabled={busy || !data.preferences.enabled} onToggle={() => change(key, !data.preferences[key])} />)}</SettingsList>
      </>}
    </div>
    <DialogFooter><Button variant="outline" disabled={busy} onClick={control.onClose}>Done</Button></DialogFooter>
  </AppDialog>;
}
