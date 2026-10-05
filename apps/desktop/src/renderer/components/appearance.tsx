import { createContext, useContext, useEffect, useLayoutEffect, type PropsWithChildren } from "react";
import { queryOptions, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Hint } from "./hint";
import type { Appearance } from "../../shared/contracts";
import { ItemContent, ItemTitle, ItemActions } from "./ui/item";
import { SettingsItem } from "./settings";
import { FieldLabel } from "./ui/field";
import { Monitor, Sun, Moon } from "lucide-react";
import { ToggleGroup, ToggleGroupItem } from "./ui/toggle-group";
import { desktopApi } from "../lib/environment";

const AppearanceContext = createContext<Appearance | null>(null);
const appearanceQuery = queryOptions({ queryKey: ["appearance"], queryFn: () => desktopApi.getAppearance() });

/**
 * Selects the document's appearance while mounted; `public/appearance-init.js`
 * resolves it to the theme. Without a selection (`system`, or once unmounted,
 * as after signing out) the theme follows the OS setting.
 */
function useAppearanceTheme(value: Appearance) {
  useLayoutEffect(() => {
    if (value === "system") return;
    const root = document.documentElement;
    root.dataset.appearance = value;
    return () => { delete root.dataset.appearance; };
  }, [value]);
}

/** The saved appearance; the desktop shell applies it natively as well. */
export function AppearanceProvider({ children }: PropsWithChildren) {
  const client = useQueryClient();
  const { data } = useQuery(appearanceQuery);
  const value = data ?? "system";
  useEffect(() => desktopApi.onAppearanceChange((next) => { void client.cancelQueries({ queryKey: appearanceQuery.queryKey }).then(() => client.setQueryData(appearanceQuery.queryKey, next)); }), [client]);
  useAppearanceTheme(value);
  return <AppearanceContext.Provider value={value}>{children}</AppearanceContext.Provider>;
}

export function useAppearance(): Appearance {
  const appearance = useContext(AppearanceContext);
  if (!appearance) throw new Error("AppearanceProvider is required");
  return appearance;
}

export function AppearanceControl() {
  const client = useQueryClient();
  const appearance = useAppearance();
  const mutation = useMutation({
    mutationFn: (next: Appearance) => desktopApi.setAppearance(next),
    onMutate: () => client.cancelQueries({ queryKey: appearanceQuery.queryKey }),
    onSuccess: (_, next) => { client.setQueryData(appearanceQuery.queryKey, next); },
    meta: { errorTitle: "Could not change the appearance" },
  });
  return <SettingsItem><ItemContent><ItemTitle><FieldLabel id="appearance-label">Theme</FieldLabel></ItemTitle></ItemContent><ItemActions>
    <ToggleGroup size="sm" variant="outline" spacing={0} aria-labelledby="appearance-label" value={[appearance]} disabled={mutation.isPending} onValueChange={([value]) => {
      if (!mutation.isPending && (value === "system" || value === "light" || value === "dark")) mutation.mutate(value);
    }}>{([["system", "System", Monitor], ["light", "Light", Sun], ["dark", "Dark", Moon]] as const).map(([value, label, Icon]) => <Hint key={value} content={label}><ToggleGroupItem value={value} aria-label={label}><Icon className="size-4" /></ToggleGroupItem></Hint>)}</ToggleGroup>
  </ItemActions></SettingsItem>;
}
