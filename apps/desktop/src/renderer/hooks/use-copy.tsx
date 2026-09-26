import { useEffect, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { desktopApi } from "../lib/environment";

type Copied = { label: string; value: string };

/**
 * Copies values to the clipboard. The value copied last shows as copied for a
 * moment, and `status`, a live region, announces it; render it in the same
 * view as the copy buttons, since an open dialog hides the page behind it from
 * screen readers. Each copy clears the region first, so copying the same value
 * again is announced again.
 */
export function useCopy(onError?: (error: unknown, label: string) => void) {
  const [copied, setCopied] = useState<Copied>();
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(undefined), 1_500);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const { mutate, isPending, error } = useMutation({
    mutationFn: ({ value }: Copied) => desktopApi.copyText(value),
    onMutate: () => setCopied(undefined),
    onSuccess: (_, copy) => setCopied(copy),
    onError: (failure, { label }) => onError?.(failure, label),
  });
  return {
    copy: (label: string, value: string) => mutate({ label, value }),
    copying: isPending,
    /** Why the last copy failed. */
    error,
    /** Whether `value` is the value copied a moment ago. */
    isCopied: (value: string | undefined) => Boolean(value) && copied?.value === value,
    status: <span className="sr-only" role="status">{copied ? `${copied.label} copied` : ""}</span>,
  };
}
