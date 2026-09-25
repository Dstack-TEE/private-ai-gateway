import { createContext, useContext, useEffect, useLayoutEffect, type PropsWithChildren } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Hint } from "./hint";
import type { Appearance, DesktopApi } from "../../shared/contracts";
import { Item, ItemContent, ItemTitle, ItemActions } from "./ui/item";
import { FieldLabel } from "./ui/field";
import { toastError } from "../lib/error-message";
import { Monitor, Sun, Moon } from "lucide-react";
import { ToggleGroup, ToggleGroupItem } from "./ui/toggle-group";

const AppearanceContext = createContext({ value: "system" as Appearance, busy: false, change: (_value: Appearance) => {} });

/**
 * Selects the document's appearance; `public/appearance-init.js` resolves it
 * to the theme, `system` following the OS setting.
 */
export function useAppearanceTheme(value: Appearance) {
  useLayoutEffect(() => {
    const root = document.documentElement;
    if (value === "system") delete root.dataset.appearance;
    else root.dataset.appearance = value;
  }, [value]);
}

/**
 * How long the hidden desktop window waits for the saved appearance. A
 * backend that has not answered by then may be hung (its requests time out
 * only after two minutes), so the window opens in the system appearance; the
 * appearance event applies the saved one once the backend answers, and the
 * failed read is retried when the window gains focus.
 */
const APPEARANCE_WAIT_MS = 3_000;

/** Rejects once `ms` have passed (`AbortSignal.timeout`). */
function withDeadline<T>(promise: Promise<T>, ms: number): Promise<T> {
  const deadline = AbortSignal.timeout(ms);
  return Promise.race([
    promise,
    new Promise<never>((_, reject) => deadline.addEventListener("abort", () => reject(deadline.reason), { once: true })),
  ]);
}

/**
 * The saved appearance. The desktop window is created hidden and shown once
 * the appearance query settles, so it opens in the saved theme when there is
 * one to read.
 */
export function AppearanceProvider({ api, children }: PropsWithChildren<{ api: DesktopApi }>) {
  const client = useQueryClient();
  // A backend that is still starting fails the read at once and sends the
  // appearance event when it answers.
  const { data, isPending } = useQuery({ queryKey: ["appearance"], queryFn: () => withDeadline(api.getAppearance(), APPEARANCE_WAIT_MS), retry: false });
  const value = data ?? "system";
  const mutation = useMutation({
    mutationFn: (next: Appearance) => api.setAppearance(next),
    onMutate: () => client.cancelQueries({ queryKey: ["appearance"] }),
    onSuccess: (_, next) => { client.setQueryData(["appearance"], next); },
    onError: (error) => toastError("Could not change the appearance", error),
  });
  const busy = mutation.isPending;
  useEffect(() => api.onAppearanceChange((next) => { void client.cancelQueries({ queryKey: ["appearance"] }).then(() => client.setQueryData(["appearance"], next)); }), [api, client]);
  useAppearanceTheme(value);
  useEffect(() => {
    if (!isPending) void api.mainWindowReady().catch((error: unknown) => toastError("Could not show the window", error));
  }, [api, isPending]);
  const change = (next: Appearance) => {
    if (busy) return;
    mutation.mutate(next);
  };
  return <AppearanceContext.Provider value={{ value, busy, change }}>{children}</AppearanceContext.Provider>;
}

export function useAppearance() { return useContext(AppearanceContext).value; }

export function AppearanceControl() {
  const appearance = useContext(AppearanceContext);
  return <Item><ItemContent><ItemTitle><FieldLabel id="appearance-label">Theme</FieldLabel></ItemTitle></ItemContent><ItemActions>
    <ToggleGroup size="sm" variant="outline" spacing={0} aria-labelledby="appearance-label" value={[appearance.value]} disabled={appearance.busy} onValueChange={([value]) => {
      if (value === "system" || value === "light" || value === "dark") appearance.change(value);
    }}>{([["system", "System", Monitor], ["light", "Light", Sun], ["dark", "Dark", Moon]] as const).map(([value, label, Icon]) => <Hint key={value} content={label}><ToggleGroupItem value={value} aria-label={label}><Icon className="size-4" /></ToggleGroupItem></Hint>)}</ToggleGroup>
  </ItemActions></Item>;
}
