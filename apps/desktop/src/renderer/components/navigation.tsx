import React from "react";
import { Link, useMatches, useRouter } from "@tanstack/react-router";
import { RotateCw } from "lucide-react";
import { brand } from "../brand/brand";
import { Badge } from "./ui/badge";
import { SidebarMenu, SidebarMenuItem, SidebarMenuButton } from "./ui/sidebar";
import { macOS, titleBarDragRegion } from "../lib/environment";
import { useShell } from "../lib/shell";
import { BrandMark } from "./brand";
import { ProtectedControl, ProtectionStatus } from "./protection";
import { toneTextClass } from "../lib/tone";
import { cn } from "../lib/utils";

/** The pages of the window: the routes under the layout that have a title. */
function usePages() {
  const { routesById } = useRouter();
  return (routesById["/_app"]?.children ?? []).flatMap((route) => {
    const { title, icon } = route.options.staticData ?? {};
    return title && icon ? [{ id: route.id, to: route.fullPath, title, icon }] : [];
  });
}

export function Sidebar(): React.JSX.Element {
  const pages = usePages();
  const current = useMatches({ select: (matches) => matches.at(-1)?.routeId });
  const { updates } = useShell();
  return (
    <aside className="sidebar min-w-0 pt-3 pr-2 pb-3 pl-2 flex flex-col gap-0.5 bg-sidebar border-r border-r-sidebar-border [&_nav]:grid [&_nav]:gap-0.5 max-[620px]:pl-2 max-[620px]:pr-2 max-[440px]:pl-1.5 max-[440px]:pr-1.5">
      {macOS && <div className="sidebar-drag relative flex-[0_0_28px]" data-tauri-drag-region />}
      <div className="sidebar-brand min-h-9.5 mt-0 mr-1.5 mb-5 ml-1.5 flex items-center gap-2.25 font-semibold whitespace-nowrap overflow-hidden [&_>_*]:pointer-events-none [&_span]:overflow-hidden [&_span]:text-ellipsis max-[780px]:[&_>_span:last-child]:text-xs max-[620px]:justify-center max-[620px]:p-0 max-[620px]:[&_>_span:last-child]:hidden" {...titleBarDragRegion}>
        <BrandMark className="brand-mark size-9" />
        <span className="sidebar-brand-copy min-w-0 flex flex-col gap-0.5 text-sm leading-4.5 [&_small]:text-xs [&_small]:leading-4 [&_small]:font-normal [&_small]:text-muted-foreground"><span>{brand.productName}</span><small>{brand.byline}</small></span>
      </div>
      <nav className="w-full" id="main-navigation" aria-label="Main navigation">
        <SidebarMenu>
        {pages.map((page) => {
          const Icon = page.icon;
          return (
            <SidebarMenuItem key={page.id}><SidebarMenuButton
              size="default"
              render={<Link to={page.to} />}
              isActive={current === page.id}
              aria-label={page.title}
            >
              <Icon size={18} aria-hidden="true" />
              <span>{page.title}</span>
            </SidebarMenuButton></SidebarMenuItem>
          );
        })}
        </SidebarMenu>
      </nav>
      {updates.ready && <div className="mt-auto pt-4">
        <Badge variant="outline" className="h-8 w-full gap-2 text-sm hover:bg-muted [&>svg]:size-4!" render={<button type="button" disabled={Boolean(updates.busy)} />} aria-label="Restart to Update" onClick={() => void updates.restart()}>
          <RotateCw aria-hidden="true" /><span className="max-[620px]:hidden">Restart to Update</span>
        </Badge>
      </div>}
    </aside>
  );
}

export function PageHeader(): React.JSX.Element {
  const { state, toggleProtection } = useShell();
  const page = useMatches({ select: (matches) => matches.at(-1) });
  const protection = state.protection;
  return (
    <header className="page-header flex-[0_0_56px] mt-0 mr-6 mb-0 ml-6 pt-2 flex items-center justify-between gap-3 [&_h1]:text-xl [&_h1]:font-semibold [&_h1]:tracking-normal [&_h1]:pointer-events-none [&_h1]:select-none max-[620px]:pl-4 max-[620px]:pr-4 max-[440px]:basis-13 max-[440px]:mt-0 max-[440px]:mr-3 max-[440px]:mb-0 max-[440px]:ml-3 max-[440px]:pt-1.75 max-[440px]:gap-2" {...titleBarDragRegion}>
      <h1 id="page-title" tabIndex={-1}>{page?.staticData.title}</h1>
      {page?.fullPath !== "/" && (
        <div className="page-protection min-w-0 ml-auto flex items-center gap-2">
          <span className={cn("page-switch-copy min-w-0 grid justify-items-end leading-4 [&_strong]:text-xs [&_.protection-status]:grid [&_.protection-status]:grid-cols-[14px_auto] [&_.protection-status]:justify-items-end [&_.protection-status]:gap-x-1.25 [&_.protection-status]:gap-y-0 [&_.protection-duration]:col-span-full", toneTextClass[protection.tone])}>
            <strong><ProtectionStatus state={state} /></strong>
          </span>
          <ProtectedControl state={state} compact iconOnly onToggle={toggleProtection} />
        </div>
      )}
    </header>
  );
}
