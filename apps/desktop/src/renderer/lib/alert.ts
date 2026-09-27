import type { AlertMessage, DesktopApi } from "../../shared/contracts";
import { errorMessage } from "./error-message.ts";

/** The alert for a failed action: what failed, and why. */
export function failureAlert(title: string, error: unknown): AlertMessage {
  return { title, message: errorMessage(error) };
}

/**
 * Shows `alert` in the system sheet when `tell` shows one (the macOS app),
 * otherwise in the window's dialog; a failure the sheet could not show still
 * shows, in the dialog.
 */
export async function presentAlert(alert: AlertMessage, tell: DesktopApi["showAlert"], inWindow: (alert: AlertMessage) => Promise<unknown>): Promise<void> {
  if (tell) {
    try {
      await tell(alert);
      return;
    } catch (error) {
      console.error("Could not show the alert sheet", error);
    }
  }
  await inWindow(alert);
}
