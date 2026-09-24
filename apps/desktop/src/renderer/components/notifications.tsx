import { createContext, useCallback, useContext, useEffect, useRef, useState, type PropsWithChildren } from "react";
import { useQuery } from "@tanstack/react-query";
import type { DesktopApi, NotificationPreferences } from "../../shared/contracts";
import { SettingsList, SettingsToggle } from "./settings";
import { AppDialog } from "./app-dialog";
import { Alert, AlertDescription } from "./ui/alert";
import { DialogFooter } from "./ui/dialog";
import { Button } from "./ui/button";

function useNotificationSettings(api: DesktopApi) {
  const { data, error: readError, refetch } = useQuery({
    queryKey: ["notifications"], queryFn: () => api.getNotificationSettings(), staleTime: 0,
  });
  const [mutationError, setError] = useState<string>();
  const error = mutationError ?? (readError ? "Could not read notification settings." : undefined);
  const [busy, setBusy] = useState(false);
  const startupRequested = useRef(false);
  useEffect(() => {
    if (!data || startupRequested.current) return;
    startupRequested.current = true;
    if (!data.preferences.enabled || data.permission !== "notDetermined") return;
    let active = true;
    void api.requestNotificationPermission().then(() => refresh()).catch(() => { if (active) setError("Could not request notification permission. Check system settings."); });
    return () => { active = false; };
  }, [api, data]);
  const refresh = useCallback(async () => {
    setError(undefined);
    try { await refetch({ throwOnError: true }); }
    catch { setError("Could not read notification settings."); }
  }, [refetch]);
  const change = async (key: keyof NotificationPreferences, enabled: boolean) => {
    if (!data || busy) return;
    setBusy(true); setError(undefined);
    try {
      await api.saveNotificationSettings({ ...data.preferences, [key]: enabled });
      let permissionFailed = false;
      if (key === "enabled" && enabled && data.permission === "notDetermined") {
        try { await api.requestNotificationPermission(); }
        catch { permissionFailed = true; }
      }
      await refresh();
      if (permissionFailed) setError("Notifications are enabled in this app, but system permission could not be requested.");
    }
    catch { setError("Could not save notification settings."); }
    finally { setBusy(false); }
  };
  const permissionAction = async () => {
    if (busy) return;
    setBusy(true); setError(undefined);
    try {
      if (data?.permission === "notDetermined") await api.requestNotificationPermission();
      else await api.openNotificationSettings();
      await refresh();
    } catch { setError("Could not open notification permissions. Check your system settings."); }
    finally { setBusy(false); }
  };
  return { data, error, busy, change, permissionAction, refresh };
}

const Context = createContext<ReturnType<typeof useNotificationSettings> | null>(null);
export function NotificationsProvider({ api, children }: PropsWithChildren<{ api: DesktopApi }>) {
  return <Context.Provider value={useNotificationSettings(api)}>{children}</Context.Provider>;
}
export function useNotifications() {
  const value = useContext(Context);
  if (!value) throw new Error("NotificationsProvider is required");
  return value;
}

function NotificationPermissionNotice() {
  const { data, busy, permissionAction } = useNotifications();
  if (!data || !data.preferences.enabled || (data.permission === "granted" && data.alertsEnabled !== false)) return null;
  const supported = data.permission !== "unsupported";
  return <Alert className="border-warning/30 bg-warning/10">
    <AlertDescription className="flex flex-wrap items-center justify-between gap-3 text-warning">
      <span>{data.permission === "granted" ? "Notifications are allowed, but banner alerts are disabled in system settings." : data.permission === "denied" ? "Notifications are disabled in system settings." : data.permission === "notDetermined" ? "System permission is needed to show notifications." : "System notification permission could not be confirmed. Check your desktop notification settings."}</span>
      {supported && <Button variant="outline" size="sm" disabled={busy} onClick={() => void permissionAction()}>{data.permission === "notDetermined" ? "Allow Notifications" : "System Settings"}</Button>}
    </AlertDescription>
  </Alert>;
}

export function NotificationsDialog({ onClose }: { onClose(): void }) {
  const { data, error, busy, change } = useNotifications();
  return <AppDialog title="Notifications" className="sm:max-w-xl" dismissible={!busy} onClose={onClose}>
    <div className="-mx-6 flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-6 py-1">
      <NotificationPermissionNotice />
      {error && <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert>}
      {data && <>
        <SettingsList><SettingsToggle label="Allow notifications" checked={data.preferences.enabled} disabled={busy} onToggle={() => void change("enabled", !data.preferences.enabled)} /></SettingsList>
        <SettingsList>{([
          ["gateway", "Protection problems", "Protection, connection and agent configuration errors."],
          ["localApi", "Local API problems", "The local listener becomes unavailable."],
          ["verification", "Response verification failures", "A response fails proof verification."],
        ] as const).map(([key, label, description]) => <SettingsToggle key={key} label={label} description={description} checked={data.preferences[key]} disabled={busy || !data.preferences.enabled} onToggle={() => void change(key, !data.preferences[key])} />)}</SettingsList>
      </>}
    </div>
    <DialogFooter><Button variant="outline" disabled={busy} onClick={onClose}>Done</Button></DialogFooter>
  </AppDialog>;
}
