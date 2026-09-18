import type { DesktopApi } from "../../shared/contracts";
import { useErrorAlert } from "../lib/error-alert";

export function ErrorAlert({ title, error, api }: { title: string; error?: unknown; api?: Pick<DesktopApi, "showErrorAlert"> }) {
  useErrorAlert(title, error, api);
  return null;
}
