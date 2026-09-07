import { useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { Download, Upload } from "lucide-react";
import type { DesktopApi } from "../../shared/contracts";
import { IconButton } from "./controls";
import { SettingsLink } from "./settings";

const mock = new URLSearchParams(window.location.search).has("mock");
const filters = [{ name: "JSON", extensions: ["json"] }];

export function ProfileTransfer({ api, disabled, onBusy, onMessage }: {
  api: DesktopApi; disabled: boolean; onBusy(busy: boolean): void; onMessage(message: string, failed: boolean): void;
}) {
  const [busy, setBusy] = useState(false);
  const run = async (importing: boolean) => {
    if (busy) return;
    setBusy(true); onBusy(true);
    try {
      if (importing) {
        const path = mock ? "profiles.json" : await open({ title: "Import Profile Configurations", multiple: false, filters });
        if (!path) return;
        const backup = await api.readProfileBackup(path);
        const names = backup.profiles.slice(0, 5).map((profile) => profile.name).join(", ");
        if (!await api.confirm({ title: `Import ${backup.profiles.length} profile configurations?`, message: `${names}${backup.profiles.length > 5 ? ", ..." : ""}\nExisting profiles will not be overwritten. Imported profiles need credentials and verification before use.`, confirmLabel: "Import" })) return;
        const result = await api.importProfiles(backup);
        onMessage(`${result.imported} imported, ${result.skipped} duplicates skipped.`, false);
      } else {
        const path = mock ? "profiles.json" : await save({ title: "Export Profile Configurations (No Keys)", defaultPath: "private-ai-gateway-profiles.json", filters });
        if (!path) return;
        await api.exportProfiles(path);
        onMessage("Profile configurations exported without credentials.", false);
      }
    } catch { onMessage(importing ? "Could not import profile configurations. Check the file format and profile limit." : "Could not export profile configurations.", true); }
    finally { setBusy(false); onBusy(false); }
  };
  return <div className="flex items-center gap-2">
    <IconButton label="Import profile configurations" disabled={disabled || busy} onClick={() => void run(true)}><Upload /></IconButton>
    <IconButton label="Export profile configurations" disabled={disabled || busy} onClick={() => void run(false)}><Download /></IconButton>
  </div>;
}

export function ExportDiagnostics({ api, onMessage }: { api: DesktopApi; onMessage(message: string): void }) {
  const [busy, setBusy] = useState(false);
  const run = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const path = mock ? "diagnostics.json" : await save({ title: "Export Redacted Diagnostics", defaultPath: "private-ai-gateway-diagnostics.json", filters });
      if (!path) return;
      await api.exportDiagnostics(path);
      onMessage("Diagnostics exported without keys, URLs, local paths or request content.");
    } catch { onMessage("Could not export diagnostics."); }
    finally { setBusy(false); }
  };
  return <SettingsLink title={busy ? "Exporting diagnostics" : "Export diagnostics"} aria-label="Export diagnostics" disabled={busy} onClick={() => void run()} />;
}
