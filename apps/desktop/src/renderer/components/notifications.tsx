import { createContext, useCallback, useContext, useEffect, useRef, useState, type PropsWithChildren } from "react";
import type { DesktopApi, NotificationConfiguration, NotificationPreferences } from "../../shared/contracts";
import { SettingsList, SettingsToggle } from "./settings";
import { Sheet, SheetActions } from "./sheet";
import { Alert, AlertDescription } from "./ui/alert";
import { Button } from "./ui/button";

function useNotificationSettings(api: DesktopApi) {
  const [data, setData] = useState<NotificationConfiguration>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const generation = useRef(0);
  const refresh = useCallback(async () => {
    const run = ++generation.current;
    try { const value = await api.getNotificationSettings(); if (run === generation.current) { setData(value); setError(undefined); } }
    catch { if (run === generation.current) setError("Could not read notification settings."); }
  }, [api]);
  useEffect(() => {
    void refresh();
    const focus = () => { void refresh(); };
    window.addEventListener("focus", focus);
    return () => { generation.current++; window.removeEventListener("focus", focus); };
  }, [refresh]);
  const change = async (key: keyof NotificationPreferences, enabled: boolean) => {
    if (!data || busy) return;
    setBusy(true); setError(undefined); generation.current++;
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
  if (!data || !data.preferences.enabled || data.permission === "granted") return null;
  const supported = data.permission !== "unsupported";
  return <Alert className="border-warning/30 bg-warning/10">
    <AlertDescription className="flex flex-wrap items-center justify-between gap-3 text-warning">
      <span>{data.permission === "denied" ? "Notifications are disabled in system settings." : data.permission === "notDetermined" ? "System permission is needed to show notifications." : "System notification permission could not be confirmed. Check your desktop notification settings."}</span>
      {supported && <Button variant="outline" size="sm" disabled={busy} onClick={() => void permissionAction()}>{data.permission === "notDetermined" ? "Allow Notifications" : "System Settings"}</Button>}
    </AlertDescription>
  </Alert>;
}

export function NotificationsSheet({ onClose }: { onClose(): void }) {
  const { data, error, busy, change, refresh } = useNotifications();
  return <Sheet title="Notifications" className="notifications-sheet" dismissible={!busy} onClose={onClose}>
    <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-auto py-4">
      <NotificationPermissionNotice />
      {error && <Alert variant="destructive"><AlertDescription>{error}<Button variant="outline" size="sm" onClick={() => void refresh()}>Retry</Button></AlertDescription></Alert>}
      {data && <>
        <SettingsList><SettingsToggle label="Allow notifications" checked={data.preferences.enabled} disabled={busy} onToggle={() => void change("enabled", !data.preferences.enabled)} /></SettingsList>
        <SettingsList>{([
          ["gateway", "Gateway problems", "Protection, connection and agent configuration errors."],
          ["localApi", "Local API problems", "The local listener becomes unavailable."],
          ["verification", "Response verification failures", "A response fails proof verification."],
        ] as const).map(([key, label, description]) => <SettingsToggle key={key} label={label} description={description} checked={data.preferences[key]} disabled={busy || !data.preferences.enabled} onToggle={() => void change(key, !data.preferences[key])} />)}</SettingsList>
      </>}
    </div>
    <SheetActions><Button variant="outline" disabled={busy} onClick={onClose}>Done</Button></SheetActions>
  </Sheet>;
}
