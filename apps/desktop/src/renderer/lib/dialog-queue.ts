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
    /**
     * The dialog finished closing; the next request, if any, shows. Returns
     * whether one did. A close while a request is shown is ignored.
     */
    closed: () => {
      if (shown) return true;
      busy = false;
      next();
      return Boolean(shown);
    },
  };
}
