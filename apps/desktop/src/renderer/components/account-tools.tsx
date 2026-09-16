import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeftRight, Ellipsis, ExternalLink } from "lucide-react";
import type { AccountBalanceTarget, AccountImages, AccountScope, DesktopApi, ServiceProvider } from "../../shared/contracts";
import { useQuery } from "@tanstack/react-query";
import { currency } from "../lib/usage-presentation";
import { Avatar, AvatarFallback, AvatarImage } from "./ui/avatar";
import { Button } from "./ui/button";
import { useErrorAlert } from "../lib/error-alert";
import { Item, ItemActions, ItemContent, ItemTitle } from "./ui/item";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "./ui/dropdown-menu";

type Props = {
  api: Pick<DesktopApi, "getAccountBalance" | "openOrganization">;
  provider: ServiceProvider;
  target: AccountBalanceTarget;
  scope?: AccountScope;
  credentialRef?: string;
  images?: AccountImages;
  onSignIn?(): void;
  disabled?: boolean;
};

type BalanceProps = Pick<Props, "provider" | "target" | "credentialRef"> & {
  api: Pick<DesktopApi, "getAccountBalance">;
  enabled: boolean;
};

function balanceCacheKey({ provider, target, credentialRef }: Pick<Props, "provider" | "target" | "credentialRef">) {
  const id = target.kind === "login" ? target.id : target.profileId;
  return `${provider}:${target.kind}:${id}:${credentialRef ?? ""}`;
}

function useAccountBalance({ api, provider, target, credentialRef, enabled = true }: Omit<BalanceProps, "enabled"> & { enabled?: boolean }) {
  const cacheKey = balanceCacheKey({ provider, target, credentialRef });
  return useQuery({
    queryKey: ["account-balance", cacheKey],
    queryFn: () => api.getAccountBalance(target),
    enabled,
    refetchInterval: enabled ? 60_000 : false,
    staleTime: 30_000,
    retry: false,
  });
}

/** Compact account balance for a profile whose credential is already in active use. */
export function AccountBalanceValue(props: BalanceProps) {
  const { data: balance, isFetching } = useAccountBalance(props);
  if (!balance) return null;
  return <span className="shrink-0 text-sm font-medium tabular-nums text-muted-foreground" aria-label={`Current balance: ${currency(Number(balance.balanceUsd))}`} role="status" aria-live="polite" aria-busy={isFetching}>
    {currency(Number(balance.balanceUsd))}
  </span>;
}

/** Changing account or credential must never display the previous account's balance. */
export function AccountTools(props: Props) {
  const cacheKey = balanceCacheKey(props);
  return <AccountDetailsView key={cacheKey} {...props} />;
}

function AccountDetailsView({ api, provider, target, scope, credentialRef, images, onSignIn, disabled = false }: Props) {
  const reportError = useErrorAlert("Could not open account");
  const [opening, setOpening] = useState(false);
  const openingRef = useRef(false);
  const returningFromAccountPage = useRef(false);
  const { data: balance, isFetching: busy, refetch } = useAccountBalance({ api, provider, target, credentialRef });
  useEffect(() => {
    const refreshAfterBilling = () => {
      if (!returningFromAccountPage.current || document.visibilityState === "hidden") return;
      returningFromAccountPage.current = false;
      void refetch();
    };
    window.addEventListener("focus", refreshAfterBilling);
    document.addEventListener("visibilitychange", refreshAfterBilling);
    return () => {
      window.removeEventListener("focus", refreshAfterBilling);
      document.removeEventListener("visibilitychange", refreshAfterBilling);
    };
  }, [refetch]);

  const openPage = useCallback(async (action: () => Promise<void>) => {
    if (openingRef.current || disabled) return;
    openingRef.current = true;
    setOpening(true);
    returningFromAccountPage.current = true;
    try { await action(); }
    catch (error) { reportError(error); returningFromAccountPage.current = false; }
    finally { openingRef.current = false; setOpening(false); }
  }, [disabled, reportError]);
  const organizationSlug = balance?.scope.organizationSlug ?? scope?.organizationSlug;
  const manage = provider === "redpill" && organizationSlug ? () => void openPage(() => api.openOrganization(organizationSlug)) : undefined;
  const displayScope = provider === "redpill" ? scope ?? balance?.scope : balance?.scope ?? scope;
  const owner = displayScope?.organization ?? displayScope?.workspace;
  const name = owner ?? "Account";
  return <div aria-label="Account details">
    <Item variant="outline" size="sm" className="grid grid-cols-[2rem_minmax(0,_1fr)_auto] gap-x-3">
      <AccountAvatar name={name} src={images?.organization} />
      <ItemContent className="min-h-8 min-w-0 justify-center">
        <ItemTitle className="line-clamp-none wrap-anywhere">{name}</ItemTitle>
      </ItemContent>
      <ItemActions>
        {balance && <span className="text-sm font-medium tabular-nums" aria-label="Balance in USD" role="status" aria-live="polite" aria-busy={busy}
          title={balance.grantedUsd != null && Number(balance.grantedUsd) > 0 ? `${currency(Number(balance.grantedUsd))} promo credits` : undefined}>
          {currency(Number(balance.balanceUsd))}
        </span>}
        <AccountActions disabled={disabled || opening} onManage={manage} onSignIn={onSignIn} />
      </ItemActions>
    </Item>
  </div>;
}

function AccountAvatar({ name, src }: { name: string; src?: string | null }) {
  const initials = name.trim().split(/\s+/).slice(0, 2).map((part) => part.charAt(0)).join("").toUpperCase();
  return <Avatar>
    {src && <AvatarImage src={src} alt={`${name} avatar`} referrerPolicy="no-referrer" />}
    <AvatarFallback>{initials}</AvatarFallback>
  </Avatar>;
}

function AccountActions({ disabled, onSignIn, onManage }: {
  disabled: boolean;
  onSignIn?(): void;
  onManage?(): void;
}) {
  const trigger = useRef<HTMLButtonElement>(null);
  const [container, setContainer] = useState<HTMLDialogElement | null>(null);
  return <DropdownMenu onOpenChange={(open) => { if (open) setContainer(trigger.current?.closest("dialog") ?? null); }}>
    <DropdownMenuTrigger render={<Button ref={trigger} type="button" size="icon-sm" variant="ghost" aria-label="Account actions" disabled={disabled} />}><Ellipsis aria-hidden /></DropdownMenuTrigger>
    <DropdownMenuContent align="end" container={container ?? undefined}>
      {onManage && <DropdownMenuItem onClick={onManage}><ExternalLink aria-hidden />Manage</DropdownMenuItem>}
      {onSignIn && <DropdownMenuItem onClick={onSignIn}><ArrowLeftRight aria-hidden />Switch</DropdownMenuItem>}
    </DropdownMenuContent>
  </DropdownMenu>;
}
