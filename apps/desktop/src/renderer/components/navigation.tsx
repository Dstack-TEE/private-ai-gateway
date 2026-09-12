import React from "react";
import { BatteryMedium, Bot, ChartNoAxesColumn, Download, LayoutGrid, Settings, Wifi } from "lucide-react";
import { brand } from "../generated/brand";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import { SidebarProvider, SidebarMenu, SidebarMenuItem, SidebarMenuButton } from "./ui/sidebar";
import type { GatewayState } from "../../shared/contracts";
import { macOS } from "../lib/environment";
import { BrandMark } from "./brand";
import { presentation, profileIsAvailable } from "../lib/protection";
import { serviceHost } from "../lib/format";
import { ProtectedControl, ProtectionStatus } from "./protection";

const TRAY_ITEM_CLASS = "preview-tray-item w-full min-h-7.5 pt-0.75 pr-3.5 pb-0.75 pl-3.5 flex items-center text-inherit bg-transparent border-0 text-left text-sm hover:text-primary-foreground hover:bg-primary hover:shadow-none focus-visible:text-primary-foreground focus-visible:bg-primary focus-visible:shadow-none [&:hover_.preview-tray-check]:text-primary-foreground [&:focus-visible_.preview-tray-check]:text-primary-foreground";

export type View = "overview" | "agents" | "usage" | "settings";

export type SettingsTarget = "confidential" | "privacy" | "local-api" | "local-api-example" | "notifications";

const VIEWS: { id: View; label: string; icon: typeof LayoutGrid }[] = [
  { id: "overview", label: "Overview", icon: LayoutGrid },
  { id: "agents", label: "Agents", icon: Bot },
  { id: "usage", label: "Usage", icon: ChartNoAxesColumn },
  { id: "settings", label: "Settings", icon: Settings },
];

export function Sidebar({
  view,
  previewControls,
  onChange,
  updateAvailable,
  updateBusy,
  onInstallUpdate,
}: {
  view: View;
  previewControls: boolean;
  updateAvailable: boolean;
  updateBusy: boolean;
  onInstallUpdate(): void;
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
        {previewControls && (
          <span className="traffic-lights absolute inset-0 p-1 flex items-center gap-2 [&_>_span]:w-3 [&_>_span]:h-3 [&_>_span]:border-[0.5px] [&_>_span]:border-[color-mix(in_srgb,_var(--color-black)_16%,_transparent)] [&_>_span]:rounded-full [&_>_span]:[box-shadow:inset_0_0_0_0.5px_color-mix(in_srgb,_var(--color-white)_18%,_transparent)] max-[440px]:top-5.5 max-[440px]:left-1/2 max-[440px]:gap-1 max-[440px]:-translate-x-1/2 max-[440px]:[&_>_span]:w-2 max-[440px]:[&_>_span]:h-2" aria-hidden="true">
            <span className="traffic-close bg-red-500" />
            <span className="traffic-minimize bg-amber-400" />
            <span className="traffic-zoom bg-green-500" />
          </span>
        )}
      </div>
      <div className="sidebar-brand min-h-9.5 mt-0 mr-1.5 mb-5 ml-1.5 flex items-center gap-2.25 font-semibold whitespace-nowrap overflow-hidden [&_>_*]:pointer-events-none [&_span]:overflow-hidden [&_span]:text-ellipsis max-[780px]:[&_>_span:last-child]:text-xs max-[620px]:justify-center max-[620px]:p-0 max-[620px]:[&_>_span:last-child]:hidden" data-tauri-drag-region>
        <BrandMark className="brand-mark w-7.5 h-7.5" />
        <span className="sidebar-brand-copy min-w-0 flex flex-col gap-0.5 leading-4.5 [&_small]:text-xs [&_small]:font-normal [&_small]:text-muted-foreground"><span>{brand.productName}</span><small>{brand.byline}</small></span>
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
      {updateAvailable && <div className="mt-auto pt-4">
        <Badge variant="outline" className="h-8 w-full gap-2 text-sm hover:bg-muted [&>svg]:size-4!" render={<button type="button" disabled={updateBusy} />} aria-label="Update available" onClick={onInstallUpdate}>
          <Download aria-hidden="true" /><span className="max-[620px]:hidden">Update available</span>
        </Badge>
      </div>}
    </aside>
  );
}

export function MacMenuBar({ protected: isProtected, trayOpen, onTray }: { protected: boolean; trayOpen: boolean; onTray(): void }): React.JSX.Element {
  const date = new Intl.DateTimeFormat("en-US", {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  }).format(new Date());
  return (
    <div className="mac-menu-bar absolute z-30 top-0 right-0 bottom-auto left-0 h-7 pt-0 pr-2.5 pb-0 pl-2.5 flex items-center justify-between gap-4 text-foreground bg-background/82 border-b border-b-[color-mix(in_srgb,_var(--color-black)_12%,_transparent)] [box-shadow:0_1px_8px_color-mix(in_srgb,_var(--color-black)_8%,_transparent)] [backdrop-filter:blur(18px)_saturate(130%)] text-sm select-none dark:text-foreground dark:bg-background/82 dark:[border-bottom-color:color-mix(in_srgb,_var(--color-white)_13%,_transparent)] max-[440px]:pt-0 max-[440px]:pr-1.75 max-[440px]:pb-0 max-[440px]:pl-1.75">
      <div className="mac-menu-left min-w-0 flex items-center gap-4.25 whitespace-nowrap [&_strong]:text-sm [&_strong]:font-semibold max-[620px]:[&_span:not(.mac-apple)]:hidden max-[620px]:[&_strong]:hidden max-[620px]:gap-0" aria-hidden="true">
        <span className="mac-apple text-xs" aria-hidden="true">◆</span>
        <strong>{brand.productName}</strong>
        <span>File</span><span>Edit</span><span>View</span><span>Window</span><span>Help</span>
      </div>
      <div className="mac-menu-right min-w-0 flex items-center whitespace-nowrap gap-2.75 [&_time]:tabular-nums max-[620px]:[&_time]:max-w-37.5 max-[620px]:[&_time]:overflow-hidden max-[620px]:[&_time]:text-ellipsis max-[440px]:gap-2 max-[440px]:[&_time]:max-w-31.5">
        <Button variant="ghost" className={`tray-trigger w-6 h-6 p-0.5 grid place-items-center bg-transparent border-0 rounded-sm hover:bg-black/10 [&.is-open]:bg-black/10 dark:hover:bg-white/13 dark:[&.is-open]:bg-white/13 ${trayOpen ? " is-open" : ""}`} aria-label="Private AI Proxy menu" aria-expanded={trayOpen} onClick={onTray}>
          <span className={`tray-template-icon w-4.5 h-4.5 text-inherit bg-current [mask:url("./generated/tray-mark.svg")_center_/_contain_no-repeat] [mask-mode:alpha] ${isProtected ? "is-protected" : "opacity-45"}`} aria-hidden="true" />
        </Button>
        <Wifi size={15} strokeWidth={1.8} aria-hidden="true" />
        <BatteryMedium size={17} strokeWidth={1.8} aria-hidden="true" />
        <time aria-hidden="true">{date}</time>
      </div>
    </div>
  );
}

export function PreviewTrayMenu({
  state,
  busy,
  running,
  endpointDown,
  developmentMode,
  openAtLogin,
  onProtection,
  onOpen,
  onSettings,
  onOpenAtLogin,
  onQuit,
  onStopAllQuit,
}: {
  state: GatewayState;
  busy: boolean;
  running: boolean;
  endpointDown: boolean;
  developmentMode: boolean;
  openAtLogin: boolean;
  onProtection(): void;
  onOpen(): void;
  onSettings(): void;
  onOpenAtLogin(): void;
  onQuit(): void;
  onStopAllQuit(): void;
}): React.JSX.Element {
  const verdict = presentation(state);
  const verifying = state.status === "verifying" && !state.configurationVerification;
  const activeProfile = state.profiles.find((profile) => profile.id === state.activeProfileId);
  const action = verifying ? "Cancel verification" : running ? "Stop protection"
    : profileIsAvailable(activeProfile, state) ? "Start protection" : "Set Up Profile…";
  return (
    <div className="preview-tray fixed z-50 top-8 right-2 w-71.5 pt-2.25 pr-0 pb-2.25 pl-0 text-foreground bg-white/94 border border-[color-mix(in_srgb,_var(--color-black)_16%,_transparent)] rounded-xl [box-shadow:0_16px_40px_color-mix(in_srgb,_var(--color-black)_30%,_transparent),_0_2px_8px_color-mix(in_srgb,_var(--color-black)_18%,_transparent)] [backdrop-filter:blur(26px)_saturate(140%)] dark:text-foreground dark:bg-card/95 dark:border-[color-mix(in_srgb,_var(--color-white)_17%,_transparent)] max-[440px]:right-2 max-[440px]:w-[min(286px,_calc(100vw_-_16px))]" role="menu" aria-label="Private AI Proxy">
      <div className="preview-tray-heading min-h-14.5 pt-1.25 pr-3.5 pb-2 pl-3.5 flex items-center gap-2.5 [&_.brand-logo]:w-8 [&_.brand-logo]:h-8 [&_span]:min-w-0 [&_span]:grid [&_strong]:text-sm [&_strong]:font-semibold [&_small]:text-muted-foreground [&_small]:text-xs dark:[&_small]:text-muted-foreground">
        <BrandMark />
        <span><strong>{brand.productName}</strong><small>{serviceHost(state.remoteUrl ?? state.config.remoteUrl)}</small></span>
      </div>
      <div className="preview-tray-status text-muted-foreground text-xs pt-1 pr-3.5 pb-1 pl-3.5 dark:text-muted-foreground" role="status">{verdict.title}{developmentMode ? " (Dev mode)" : ""}</div>
      <Button variant="ghost" className={TRAY_ITEM_CLASS} role="menuitem" disabled={(busy && !verifying) || (endpointDown && !running && !verifying)} onClick={onProtection}>{action}</Button>
      <div className="preview-tray-separator h-px mt-1.25 mr-3.25 mb-1.25 ml-3.25 bg-black/12 dark:bg-white/13" />
      <Button variant="ghost" className={TRAY_ITEM_CLASS} role="menuitem" onClick={onOpen}>Open {brand.productName}</Button>
      <Button variant="ghost" className={TRAY_ITEM_CLASS} role="menuitem" onClick={onSettings}>Settings…</Button>
      <div className="preview-tray-separator h-px mt-1.25 mr-3.25 mb-1.25 ml-3.25 bg-black/12 dark:bg-white/13" />
      <Button variant="ghost" className={TRAY_ITEM_CLASS} role="menuitemcheckbox" aria-checked={openAtLogin} onClick={onOpenAtLogin}>
        <span className="preview-tray-check w-4.5 flex-none text-primary font-bold dark:text-primary" aria-hidden="true">{openAtLogin ? "✓" : ""}</span>
        Open at Login
      </Button>
      <Button variant="ghost" className={TRAY_ITEM_CLASS} role="menuitem" onClick={onQuit}>Quit {brand.productName}</Button>
      <Button variant="ghost" className={TRAY_ITEM_CLASS} role="menuitem" onClick={onStopAllQuit}>Stop All and Quit…</Button>
    </div>
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
  state: GatewayState;
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
          {developmentMode && <span className="state inline-flex items-center gap-1.25 text-muted-foreground text-xs font-medium [&_.dot]:w-1.5 [&_.dot]:h-1.5 [&_.dot]:flex-[0_0_6px] [&_.dot]:bg-current [&_.dot]:rounded-full state-warning text-warning">Dev mode</span>}
          <span className={`[&.state-success]:text-primary [&.state-neutral]:text-muted-foreground [&.state-warning]:text-warning [&.state-danger]:text-destructive page-switch-copy min-w-0 grid justify-items-end leading-4 [&_strong]:text-xs [&_small]:text-xs [&_small]:text-muted-foreground [&_small.is-on]:text-primary [&_small.is-development]:text-warning [&_small.is-error]:text-destructive [&_.protection-status]:grid [&_.protection-status]:grid-cols-[14px_auto] [&_.protection-status]:justify-items-end [&_.protection-status]:gap-x-1.25 [&_.protection-status]:gap-y-0 [&_.protection-duration]:col-span-full state-${verdict.tone}`}>
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
