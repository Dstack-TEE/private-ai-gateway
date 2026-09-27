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

/**
 * Questions and alerts for one dialog, shown one at a time in the order they
 * were asked: a new one waits until the dialog showing the previous one has
 * answered and closed, so an alert never cancels a question and a question
 * never replaces an unread alert.
 */
export function createDialogQueue<T>(show: (request: T) => void, hide: () => void) {
  const waiting: { request: T; resolve(confirmed: boolean): void }[] = [];
  let shown: (typeof waiting)[number] | undefined;
  let busy = false;
  const next = () => {
    if (busy) return;
    shown = waiting.shift();
    if (!shown) return;
    busy = true;
    show(shown.request);
  };
  return {
    /** Resolves with the answer once the request has been shown and answered. */
    ask: (request: T) => new Promise<boolean>((resolve) => {
      waiting.push({ request, resolve });
      next();
    }),
    /** Answers the request on screen; the dialog closes. */
    answer: (confirmed: boolean) => {
      if (!shown) return;
      shown.resolve(confirmed);
      shown = undefined;
      hide();
    },
    /** The dialog finished closing; the next request, if any, shows. */
    closed: () => {
      busy = false;
      next();
    },
  };
}
