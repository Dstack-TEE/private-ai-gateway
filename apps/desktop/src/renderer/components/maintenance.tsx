import { useMutation } from "@tanstack/react-query";
import { Download, Upload } from "lucide-react";
import { toast } from "sonner";
import type { DesktopApi } from "../../shared/contracts";
import { IconButton } from "./controls";
import { SettingsLink } from "./settings";
import { useConfirm } from "./confirm";
import { errorMessage, toastError } from "../lib/error-message";

/** The mutation key of an import or export, which keeps the Profiles dialog open. */
export const PROFILE_TRANSFER = ["profile-transfer"];

export function ProfileTransfer({ api, disabled, onResult }: {
  api: DesktopApi; disabled: boolean; onResult(message: string, failed: boolean): void;
}) {
  const confirm = useConfirm();
  const transfer = useMutation({
    mutationKey: PROFILE_TRANSFER,
    mutationFn: async (importing: boolean): Promise<string | undefined> => {
      if (!importing) return await api.saveProfileExport() ? "Profile configurations exported without credentials." : undefined;
      const backup = await api.selectProfileBackup();
      if (!backup) return undefined;
      const names = backup.profiles.slice(0, 5).map((profile) => profile.name).join(", ");
      if (!await confirm({ title: `Import ${backup.profiles.length} profile configurations?`, message: `${names}${backup.profiles.length > 5 ? ", …" : ""}\nExisting profiles will not be overwritten. Imported profiles need credentials and verification before use.`, confirmLabel: "Import" })) return undefined;
      const result = await api.importProfiles(backup);
      return `${result.imported} imported, ${result.skipped} duplicates skipped.`;
    },
    onSuccess: (message) => { if (message) onResult(message, false); },
    onError: (error, importing) => onResult(`${importing ? "Could not import profile configurations." : "Could not export profile configurations."} ${errorMessage(error)}`, true),
  });
  return <div className="flex items-center gap-2">
    <IconButton label="Import profile configurations" disabled={disabled || transfer.isPending} onClick={() => transfer.mutate(true)}><Upload /></IconButton>
    <IconButton label="Export profile configurations" disabled={disabled || transfer.isPending} onClick={() => transfer.mutate(false)}><Download /></IconButton>
  </div>;
}

export function ExportDiagnostics({ api }: { api: DesktopApi }) {
  const exportDiagnostics = useMutation({
    mutationFn: () => api.saveDiagnosticsExport(),
    onSuccess: (saved) => { if (saved) toast.success("Diagnostics exported", { description: "Without keys, URLs, local paths or request content." }); },
    onError: (error) => toastError("Could not export diagnostics", error),
  });
  return <SettingsLink title={exportDiagnostics.isPending ? "Exporting diagnostics" : "Export diagnostics"} aria-label="Export diagnostics" disabled={exportDiagnostics.isPending} onClick={() => exportDiagnostics.mutate()} />;
}
