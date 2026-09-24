import { toast } from "sonner";

export function errorMessage(error: unknown): string {
  const raw = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  const message = raw.replace(/^[a-z][a-z0-9_]*:\s+/, "");
  if (!message || /undefined|desktop bridge/i.test(message)) {
    return "Desktop bridge unavailable";
  }
  return message;
}

/** Reports a failed action that has no form or page area to explain it. */
export function toastError(title: string, error: unknown): void {
  toast.error(title, { description: errorMessage(error) });
}
