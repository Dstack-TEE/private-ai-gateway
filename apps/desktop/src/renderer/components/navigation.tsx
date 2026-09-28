import React, { type RefObject } from "react";
import { Link, useMatches, useRouter } from "@tanstack/react-router";
import { RotateCw } from "lucide-react";
import { brand } from "../brand/brand";
import { Button } from "./ui/button";
import { SidebarMenu, SidebarMenuItem, SidebarMenuButton } from "./ui/sidebar";
import { overlaidTitleBar, titleBarDragRegion } from "../lib/environment";
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

export function Sidebar({ navigationRef }: { navigationRef: RefObject<HTMLElement | null> }): React.JSX.Element {
  const pages = usePages();
  const current = useMatches({ select: (matches) => matches.at(-1)?.routeId });
  const { updates } = useShell();
  return (
    <aside className="flex min-w-0 flex-col gap-0.5 border-r border-r-sidebar-border bg-sidebar px-2 py-3 max-[440px]:px-1.5">
      {overlaidTitleBar && <div className="relative flex-[0_0_28px]" data-tauri-drag-region />}
      {/* The brand drags the window where the title bar is overlaid, so its parts take no pointer events. */}
      <div className="mx-1.5 mb-5 flex min-h-9.5 items-center gap-2.25 overflow-hidden font-semibold whitespace-nowrap max-[620px]:justify-center" {...titleBarDragRegion}>
        <BrandMark className="pointer-events-none size-9" />
        <span className="pointer-events-none flex min-w-0 flex-col gap-0.5 overflow-hidden text-sm leading-4.5 text-ellipsis max-[780px]:text-xs max-[620px]:hidden">
          <span className="truncate">{brand.productName}</span>
          <small className="text-xs leading-4 font-normal text-muted-foreground">{brand.byline}</small>
        </span>
      </div>
      <nav ref={navigationRef} className="grid w-full gap-0.5" aria-label="Main navigation">
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
      {updates.ready && <div className="mt-auto flex justify-center-safe pt-4">
        <Button type="button" variant="outline" size="sm" disabled={Boolean(updates.busy)} aria-label="Restart to Update" onClick={updates.restart}>
          <RotateCw aria-hidden="true" /><span className="max-[620px]:hidden">Restart to Update</span>
        </Button>
      </div>}
    </aside>
  );
}

export function PageHeader({ titleRef }: { titleRef: RefObject<HTMLHeadingElement | null> }): React.JSX.Element {
  const { state, protectionPending, toggleProtection } = useShell();
  const page = useMatches({ select: (matches) => matches.at(-1) });
  const protection = state.protection;
  return (
    <header className="mx-6 flex flex-[0_0_56px] items-center justify-between gap-3 pt-2 max-[620px]:px-4 max-[440px]:mx-3 max-[440px]:basis-13 max-[440px]:gap-2 max-[440px]:pt-1.75" {...titleBarDragRegion}>
      <h1 ref={titleRef} tabIndex={-1} className="pointer-events-none text-xl font-semibold select-none">{page?.staticData.title}</h1>
      {page?.fullPath !== "/" && (
        <div className="ml-auto flex min-w-0 items-center gap-2">
          <span className={cn("grid min-w-0 justify-items-end leading-4", toneTextClass[protection.tone])}>
            <strong className="text-xs"><ProtectionStatus state={state} variant="header" /></strong>
          </span>
          <ProtectedControl state={state} pending={protectionPending} compact onToggle={toggleProtection} />
        </div>
      )}
    </header>
  );
}
