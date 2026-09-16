import { useCallback, useEffect, useRef } from "react";
import type { DesktopApi } from "../../shared/contracts";
import { desktopApi } from "./environment";
import { errorMessage } from "./error-message";

type ErrorApi = Pick<DesktopApi, "showErrorAlert">;
const queues = new WeakMap<ErrorApi, { tail: Promise<void>; pending: Set<string> }>();

/** Serialize native alerts and coalesce the same failure reported by multiple surfaces. */
export function showErrorAlert(title: string, error: unknown, api: ErrorApi = desktopApi): Promise<void> {
  const message = errorMessage(error);
  const key = `${title}\0${message}`;
  let queue = queues.get(api);
  if (!queue) {
    queue = { tail: Promise.resolve(), pending: new Set() };
    queues.set(api, queue);
  }
  if (queue.pending.has(key)) return queue.tail;
  const current = queue;
  current.pending.add(key);
  current.tail = current.tail.then(() => api.showErrorAlert(title, message))
    .catch(() => { console.error("Could not present the system error alert."); })
    .finally(() => { current.pending.delete(key); });
  return current.tail;
}

/** Observe one surface-local failure; the callback reports each explicit failed action. */
export function useErrorAlert(title: string, error?: unknown, api: ErrorApi = desktopApi) {
  const message = error ? errorMessage(error) : undefined;
  const previous = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (previous.current === message) return;
    previous.current = message;
    if (message) void showErrorAlert(title, message, api);
  }, [api, message, title]);
  return useCallback((failure: unknown) => { void showErrorAlert(title, failure, api); }, [api, title]);
}
