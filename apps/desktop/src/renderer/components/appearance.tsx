import { createContext, useContext, useEffect, useLayoutEffect, type PropsWithChildren } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { initialAppearance } from "../desktop-api";
import { Hint } from "./hint";
import type { Appearance, DesktopApi } from "../../shared/contracts";
import { Item, ItemContent, ItemTitle, ItemActions } from "./ui/item";
import { FieldLabel, FieldError } from "./ui/field";
import { Monitor, Sun, Moon } from "lucide-react";
import { ToggleGroup, ToggleGroupItem } from "./ui/toggle-group";

const AppearanceContext = createContext({ value: "system" as Appearance, ready: false, busy: false, error: "", change: (_value: Appearance) => {} });

export function AppearanceProvider({ api, children }: PropsWithChildren<{ api: DesktopApi }>) {
  const client = useQueryClient();
  const { data, error: readError, isPending } = useQuery({
    queryKey: ["appearance"], queryFn: () => api.getAppearance(), initialData: initialAppearance,
  });
  const value = data ?? initialAppearance ?? "system";
  const ready = initialAppearance !== undefined || !isPending;
  const mutation = useMutation({
    mutationFn: (next: Appearance) => api.setAppearance(next),
    onMutate: () => client.cancelQueries({ queryKey: ["appearance"] }),
    onSuccess: (_, next) => { client.setQueryData(["appearance"], next); },
  });
  const busy = mutation.isPending;
  const error = mutation.error ? "Could not save appearance settings." : readError ? "Could not read appearance settings." : "";
  useEffect(() => api.onAppearanceChange((next) => { void client.cancelQueries({ queryKey: ["appearance"] }).then(() => client.setQueryData(["appearance"], next)); }), [api, client]);
  useLayoutEffect(() => {
    document.documentElement.dataset.appearance = value;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const theme = value === "system" ? media.matches ? "dark" : "light" : value;
      document.documentElement.classList.toggle("dark", theme === "dark");
      document.documentElement.dataset.theme = theme;
    };
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [value]);
  const change = async (next: Appearance) => {
    if (busy) return;
    try { await mutation.mutateAsync(next); }
    catch { /* The mutation error is rendered by AppearanceControl. */ }
  };
  return <AppearanceContext.Provider value={{ value, ready, busy, error, change: (next) => void change(next) }}>{children}</AppearanceContext.Provider>;
}

export function useAppearance() { return useContext(AppearanceContext).value; }
export function useAppearanceReady() { return useContext(AppearanceContext).ready; }

export function AppearanceControl() {
  const appearance = useContext(AppearanceContext);
  return <Item><ItemContent><ItemTitle><FieldLabel id="appearance-label">Theme</FieldLabel></ItemTitle><FieldError>{appearance.error}</FieldError></ItemContent><ItemActions>
    <ToggleGroup size="sm" variant="outline" spacing={0} aria-labelledby="appearance-label" value={[appearance.value]} disabled={appearance.busy} onValueChange={([value]) => {
      if (value === "system" || value === "light" || value === "dark") appearance.change(value);
    }}>{([["system", "System", Monitor], ["light", "Light", Sun], ["dark", "Dark", Moon]] as const).map(([value, label, Icon]) => <Hint key={value} content={label}><ToggleGroupItem value={value} aria-label={label}><Icon className="size-4" /></ToggleGroupItem></Hint>)}</ToggleGroup>
  </ItemActions></Item>;
}
