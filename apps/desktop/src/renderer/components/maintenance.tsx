import { useState } from "react";
import { Download, Upload } from "lucide-react";
import type { DesktopApi } from "../../shared/contracts";
import { IconButton } from "./controls";
import { SettingsLink } from "./settings";
import { useConfirm } from "./confirm";
import { toastError } from "../lib/error-message";

export function ProfileTransfer({ api, disabled, onBusy, onMessage }: {
  api: DesktopApi; disabled: boolean; onBusy(busy: boolean): void; onMessage(message: string, failed: boolean): void;
}) {
  const [busy, setBusy] = useState(false);
  const confirm = useConfirm();
  const run = async (importing: boolean) => {
    if (busy) return;
    setBusy(true); onBusy(true);
    try {
      if (importing) {
        const backup = await api.selectProfileBackup();
        if (!backup) return;
        const names = backup.profiles.slice(0, 5).map((profile) => profile.name).join(", ");
        if (!await confirm({ title: `Import ${backup.profiles.length} profile configurations?`, message: `${names}${backup.profiles.length > 5 ? ", ..." : ""}\nExisting profiles will not be overwritten. Imported profiles need credentials and verification before use.`, confirmLabel: "Import" })) return;
        const result = await api.importProfiles(backup);
        onMessage(`${result.imported} imported, ${result.skipped} duplicates skipped.`, false);
      } else {
        await api.saveProfileExport();
        onMessage("Profile configurations exported without credentials.", false);
      }
    } catch { onMessage(importing ? "Could not import profile configurations. Check the file format and profile limit." : "Could not export profile configurations. Choose a new file name and check write permissions.", true); }
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
      await api.saveDiagnosticsExport();
      onMessage("Diagnostics exported without keys, URLs, local paths or request content.");
    } catch { toastError("Could not export diagnostics", "Choose a new file name and check write permissions."); }
    finally { setBusy(false); }
  };
  return <SettingsLink title={busy ? "Exporting diagnostics" : "Export diagnostics"} aria-label="Export diagnostics" disabled={busy} onClick={() => void run()} />;
}
