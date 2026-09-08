import { createContext, useContext, useEffect, useLayoutEffect, useState, type PropsWithChildren } from "react";
import { initialAppearance } from "../desktop-api";
import { Hint } from "./hint";
import type { Appearance, DesktopApi } from "../../shared/contracts";
import { Item, ItemContent, ItemTitle, ItemActions } from "./ui/item";
import { FieldLabel, FieldError } from "./ui/field";
import { Monitor, Sun, Moon } from "lucide-react";
import { ToggleGroup, ToggleGroupItem } from "./ui/toggle-group";

const AppearanceContext = createContext({ value: "system" as Appearance, busy: false, error: "", change: (_value: Appearance) => {} });

export function AppearanceProvider({ api, children }: PropsWithChildren<{ api: DesktopApi }>) {
  const [value, setValue] = useState<Appearance>(initialAppearance ?? "system");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    let active = true;
    let received = false;
    const unsubscribe = api.onAppearanceChange((next) => { received = true; if (active) setValue(next); });
    void api.getAppearance().then((next) => { if (active && !received) setValue(next); }).catch(() => { if (active) setError("Could not read appearance settings."); });
    return () => { active = false; unsubscribe(); };
  }, [api]);
  useLayoutEffect(() => {
    document.documentElement.dataset.appearance = value;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => { document.documentElement.dataset.theme = value === "system" ? media.matches ? "dark" : "light" : value; };
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [value]);
  const change = async (next: Appearance) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try { await api.setAppearance(next); setValue(next); }
    catch { setError("Could not save appearance settings."); }
    finally { setBusy(false); }
  };
  return <AppearanceContext.Provider value={{ value, busy, error, change: (next) => void change(next) }}>{children}</AppearanceContext.Provider>;
}

export function useAppearance() { return useContext(AppearanceContext).value; }

export function AppearanceControl() {
  const appearance = useContext(AppearanceContext);
  return <Item><ItemContent><ItemTitle><FieldLabel id="appearance-label">Theme</FieldLabel></ItemTitle><FieldError>{appearance.error}</FieldError></ItemContent><ItemActions>
    <ToggleGroup size="sm" variant="outline" spacing={0} aria-labelledby="appearance-label" value={[appearance.value]} disabled={appearance.busy} onValueChange={([value]) => {
      if (value === "system" || value === "light" || value === "dark") appearance.change(value);
    }}>{([["system", "System", Monitor], ["light", "Light", Sun], ["dark", "Dark", Moon]] as const).map(([value, label, Icon]) => <Hint key={value} content={label}><ToggleGroupItem value={value} aria-label={label}><Icon className="size-4" /></ToggleGroupItem></Hint>)}</ToggleGroup>
  </ItemActions></Item>;
}
