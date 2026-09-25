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

/** Applies an appearance to the document: `system` follows the OS setting. */
export function useAppearanceTheme(value: Appearance) {
  useLayoutEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => document.documentElement.classList.toggle("dark", value === "dark" || (value === "system" && media.matches));
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [value]);
}

/**
 * The saved appearance. The desktop window is created hidden and shown once
 * the saved appearance is applied, so it never opens in the wrong theme.
 */
export function AppearanceProvider({ api, children }: PropsWithChildren<{ api: DesktopApi }>) {
  const client = useQueryClient();
  // A backend that is still starting answers later with an appearance event;
  // the window shows in the system appearance meanwhile.
  const { data, isPending } = useQuery({ queryKey: ["appearance"], queryFn: () => api.getAppearance(), retry: false });
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
