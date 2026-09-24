import React from "react";
import { Bot, ChartNoAxesColumn, LayoutGrid, RotateCw, Settings } from "lucide-react";
import { brand } from "../generated/brand";
import { Badge } from "./ui/badge";
import { SidebarProvider, SidebarMenu, SidebarMenuItem, SidebarMenuButton } from "./ui/sidebar";
import type { AppState } from "../../shared/contracts";
import { macOS } from "../lib/environment";
import { BrandMark } from "./brand";
import { presentation } from "../lib/protection";
import { ProtectedControl, ProtectionStatus } from "./protection";
import { toneTextClass } from "../lib/tone";
import { cn } from "../lib/utils";

export type View = "overview" | "agents" | "usage" | "settings";

export type SettingsTarget = "confidential" | "privacy" | "local-api" | "local-api-example" | "notifications" | "web-ui";

const VIEWS: { id: View; label: string; icon: typeof LayoutGrid }[] = [
  { id: "overview", label: "Overview", icon: LayoutGrid },
  { id: "agents", label: "Agents", icon: Bot },
  { id: "usage", label: "Usage", icon: ChartNoAxesColumn },
  { id: "settings", label: "Settings", icon: Settings },
];

export function Sidebar({
  view,
  onChange,
  updateReady,
  updateBusy,
  onRestartUpdate,
}: {
  view: View;
  updateReady: boolean;
  updateBusy: boolean;
  onRestartUpdate(): void;
  onChange(view: View, focusHeading?: boolean): void;
}): React.JSX.Element {
  const onKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    const index = VIEWS.findIndex((entry) => entry.id === view);
    const step = event.key === "ArrowDown" ? 1 : event.key === "ArrowUp" ? -1 : 0;
    if (step === 0) {
      return;
    }
    event.preventDefault();
    const next = VIEWS[(index + step + VIEWS.length) % VIEWS.length]?.id ?? view;
    onChange(next, false);
    (event.currentTarget.querySelector(`#nav-${next}`) as HTMLElement | null)?.focus();
  };
  return (
    <aside className={macOS ? "sidebar min-w-0 pt-3 pr-2 pb-3 pl-2 flex flex-col gap-0.5 bg-sidebar border-r border-r-sidebar-border [&_nav]:grid [&_nav]:gap-0.5 max-[620px]:pl-2 max-[620px]:pr-2 max-[440px]:pl-1.5 max-[440px]:pr-1.5" : "sidebar min-w-0 pt-3 pr-2 pb-3 pl-2 flex flex-col gap-0.5 bg-sidebar border-r border-r-sidebar-border [&_nav]:grid [&_nav]:gap-0.5 max-[620px]:pl-2 max-[620px]:pr-2 max-[440px]:pl-1.5 max-[440px]:pr-1.5 sidebar-standard [&_.sidebar-drag]:hidden"}>
      <div className="sidebar-drag relative flex-[0_0_28px]" data-tauri-drag-region>
      </div>
      <div className="sidebar-brand min-h-9.5 mt-0 mr-1.5 mb-5 ml-1.5 flex items-center gap-2.25 font-semibold whitespace-nowrap overflow-hidden [&_>_*]:pointer-events-none [&_span]:overflow-hidden [&_span]:text-ellipsis max-[780px]:[&_>_span:last-child]:text-xs max-[620px]:justify-center max-[620px]:p-0 max-[620px]:[&_>_span:last-child]:hidden" data-tauri-drag-region>
        <BrandMark className="brand-mark size-9" />
        <span className="sidebar-brand-copy min-w-0 flex flex-col gap-0.5 text-sm leading-4.5 [&_small]:text-xs [&_small]:leading-4 [&_small]:font-normal [&_small]:text-muted-foreground"><span>{brand.productName}</span><small>{brand.byline}</small></span>
      </div>
      <SidebarProvider keyboardShortcut={false} className="min-h-0 flex-col">
      <nav className="w-full" aria-label="Main navigation" onKeyDown={onKeyDown}>
        <SidebarMenu>
        {VIEWS.map((entry) => {
          const Icon = entry.icon;
          return (
            <SidebarMenuItem key={entry.id}><SidebarMenuButton
              size="default"
              isActive={view === entry.id}
              id={`nav-${entry.id}`}
              aria-label={entry.label}
              aria-current={view === entry.id ? "page" : undefined}
              tabIndex={view === entry.id ? 0 : -1}
              onClick={() => onChange(entry.id, true)}
            >
              <Icon size={18} aria-hidden="true" />
              <span>{entry.label}</span>
            </SidebarMenuButton></SidebarMenuItem>
          );
        })}
        </SidebarMenu>
      </nav>
      </SidebarProvider>
      {updateReady && <div className="mt-auto pt-4">
        <Badge variant="outline" className="h-8 w-full gap-2 text-sm hover:bg-muted [&>svg]:size-4!" render={<button type="button" disabled={updateBusy} />} aria-label="Restart to update" onClick={onRestartUpdate}>
          <RotateCw aria-hidden="true" /><span className="max-[620px]:hidden">Restart to update</span>
        </Badge>
      </div>}
    </aside>
  );
}

export function PageHeader({
  view,
  state,
  busy,
  running,
  endpointDown,
  developmentMode,
  onToggle,
}: {
  view: View;
  state: AppState;
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  onToggle(): void;
}): React.JSX.Element {
  const title = VIEWS.find((entry) => entry.id === view)?.label ?? "";
  const verdict = presentation(state);
  return (
    <header className="page-header flex-[0_0_56px] mt-0 mr-6 mb-0 ml-6 pt-2 flex items-center justify-between gap-3 [&_h1]:text-xl [&_h1]:font-semibold [&_h1]:tracking-normal [&_h1]:pointer-events-none [&_h1]:select-none max-[620px]:pl-4 max-[620px]:pr-4 max-[440px]:basis-13 max-[440px]:mt-0 max-[440px]:mr-3 max-[440px]:mb-0 max-[440px]:ml-3 max-[440px]:pt-1.75 max-[440px]:gap-2" data-tauri-drag-region>
      <h1 id={`page-title-${view}`} tabIndex={-1}>{title}</h1>
      {view !== "overview" && (
        <div className="page-protection min-w-0 ml-auto flex items-center gap-2">
          {developmentMode && <span className="inline-flex items-center gap-1.25 text-xs font-medium text-warning">Dev mode</span>}
          <span className={cn("page-switch-copy min-w-0 grid justify-items-end leading-4 [&_strong]:text-xs [&_.protection-status]:grid [&_.protection-status]:grid-cols-[14px_auto] [&_.protection-status]:justify-items-end [&_.protection-status]:gap-x-1.25 [&_.protection-status]:gap-y-0 [&_.protection-duration]:col-span-full", toneTextClass[verdict.tone])}>
            <strong><ProtectionStatus state={state} label={verdict.title} /></strong>
          </span>
          <ProtectedControl
            state={state}
            busy={busy}
            running={running}
            endpointDown={endpointDown}
            developmentMode={developmentMode}
            compact
            iconOnly
            onToggle={onToggle}
          />
        </div>
      )}
    </header>
  );
}
