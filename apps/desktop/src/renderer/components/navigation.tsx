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
    <aside className="flex min-w-0 flex-col gap-0.5 border-r border-sidebar-border bg-sidebar px-2 py-3 max-[440px]:px-1.5">
      {overlaidTitleBar && <div className="h-7 shrink-0" data-tauri-drag-region />}
      <div className="mx-1.5 mb-5 flex min-h-9.5 items-center gap-2 overflow-hidden font-semibold whitespace-nowrap max-[620px]:mx-0 max-[620px]:justify-center [&>*]:pointer-events-none" {...titleBarDragRegion}>
        <BrandMark className="size-9" />
        <span className="flex min-w-0 flex-col gap-0.5 leading-4.5 max-[780px]:text-xs max-[620px]:hidden">
          <span className="truncate">{brand.productName}</span>
          <small className="truncate text-xs leading-4 font-normal text-muted-foreground">{brand.byline}</small>
        </span>
      </div>
      <nav ref={navigationRef} aria-label="Main navigation">
        <SidebarMenu>
        {pages.map((page) => {
          const Icon = page.icon;
          return (
            <SidebarMenuItem key={page.id}><SidebarMenuButton render={<Link to={page.to} />} isActive={current === page.id}>
              <Icon aria-hidden="true" />
              <span>{page.title}</span>
            </SidebarMenuButton></SidebarMenuItem>
          );
        })}
        </SidebarMenu>
      </nav>
      {updates.ready && <div className="mt-auto pt-4">
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
  return (
    <header className="mx-6 flex h-14 shrink-0 items-center justify-between gap-3 pt-2 max-[780px]:mx-4 max-[440px]:mx-3" {...titleBarDragRegion}>
      <h1 ref={titleRef} tabIndex={-1} className="pointer-events-none text-xl font-semibold">{page?.staticData.title}</h1>
      {page?.fullPath !== "/" && (
        <div className="flex min-w-0 items-center gap-2">
          <strong className="min-w-0 text-xs"><ProtectionStatus state={state} /></strong>
          <ProtectedControl state={state} pending={protectionPending} compact onToggle={toggleProtection} />
        </div>
      )}
    </header>
  );
}
