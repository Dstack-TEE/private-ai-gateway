import { createContext, useContext, useEffect, useLayoutEffect, type PropsWithChildren } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Hint } from "./hint";
import type { Appearance, DesktopApi } from "../../shared/contracts";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions } from "./ui/item";
import { FieldLabel } from "./ui/field";
import { errorMessage } from "../lib/error-message";
import { Monitor, Sun, Moon } from "lucide-react";
import { ToggleGroup, ToggleGroupItem } from "./ui/toggle-group";

const AppearanceContext = createContext<Appearance | null>(null);

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
export function AppearanceProvider({ api, children }: PropsWithChildren<{ api: DesktopApi }>) {
  const client = useQueryClient();
  const { data } = useQuery({ queryKey: ["appearance"], queryFn: () => api.getAppearance() });
  const value = data ?? "system";
  useEffect(() => api.onAppearanceChange((next) => { void client.cancelQueries({ queryKey: ["appearance"] }).then(() => client.setQueryData(["appearance"], next)); }), [api, client]);
  useAppearanceTheme(value);
  return <AppearanceContext.Provider value={value}>{children}</AppearanceContext.Provider>;
}

export function useAppearance(): Appearance {
  const appearance = useContext(AppearanceContext);
  if (!appearance) throw new Error("AppearanceProvider is required");
  return appearance;
}

export function AppearanceControl({ api }: { api: DesktopApi }) {
  const client = useQueryClient();
  const appearance = useAppearance();
  const mutation = useMutation({
    mutationFn: (next: Appearance) => api.setAppearance(next),
    // The appearance the change started from: a failure shows until it changes.
    onMutate: async () => {
      await client.cancelQueries({ queryKey: ["appearance"] });
      return appearance;
    },
    onSuccess: (_, next) => { client.setQueryData(["appearance"], next); },
  });
  const error = mutation.error && mutation.context === appearance ? `Could not change the appearance. ${errorMessage(mutation.error)}` : undefined;
  return <Item><ItemContent><ItemTitle><FieldLabel id="appearance-label">Theme</FieldLabel></ItemTitle>{error && <ItemDescription role="alert" className="text-destructive">{error}</ItemDescription>}</ItemContent><ItemActions>
    <ToggleGroup size="sm" variant="outline" spacing={0} aria-labelledby="appearance-label" value={[appearance]} disabled={mutation.isPending} onValueChange={([value]) => {
      if (!mutation.isPending && (value === "system" || value === "light" || value === "dark")) mutation.mutate(value);
    }}>{([["system", "System", Monitor], ["light", "Light", Sun], ["dark", "Dark", Moon]] as const).map(([value, label, Icon]) => <Hint key={value} content={label}><ToggleGroupItem value={value} aria-label={label}><Icon className="size-4" /></ToggleGroupItem></Hint>)}</ToggleGroup>
  </ItemActions></Item>;
}
