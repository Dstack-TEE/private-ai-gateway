import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeftRight, Ellipsis, ExternalLink } from "lucide-react";
import type { AccountBalance, AccountBalanceTarget, AccountImages, AccountScope, DesktopApi, ServiceProvider } from "../../shared/contracts";
import { useQuery } from "@tanstack/react-query";
import { currency } from "../lib/usage-presentation";
import { Avatar, AvatarFallback, AvatarImage } from "./ui/avatar";
import { Button } from "./ui/button";
import { toastError } from "../lib/error-message";
import { Item, ItemActions, ItemContent, ItemTitle } from "./ui/item";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "./ui/dropdown-menu";

import { distributionCapabilities } from "../lib/environment";

type Props = {
  api: Pick<DesktopApi, "getAccountBalance" | "openOrganization" | "openTopUp">;
  provider: ServiceProvider;
  target: AccountBalanceTarget;
  scope?: AccountScope;
  credentialRef?: string;
  images?: AccountImages;
  onSignIn?(): void;
  disabled?: boolean;
};

type BalanceIdentity = Pick<Props, "provider" | "target" | "credentialRef">;
type BalanceQueryProps = BalanceIdentity & {
  api: Pick<DesktopApi, "getAccountBalance">;
};
type BalanceProps = BalanceIdentity & {
  api: Pick<DesktopApi, "getAccountBalance" | "openTopUp">;
  enabled: boolean;
};

function balanceCacheKey({ provider, target, credentialRef }: Pick<Props, "provider" | "target" | "credentialRef">) {
  const id = target.kind === "login" ? target.id : target.profileId;
  return `${provider}:${target.kind}:${id}:${credentialRef ?? ""}`;
}

function useAccountBalance({ api, provider, target, credentialRef, enabled = true }: BalanceQueryProps & { enabled?: boolean }) {
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
  const { data: balance, isFetching, refetch } = useAccountBalance(props);
  const { opening, openPage } = useAccountPage("Could not open billing", refetch);
  if (!balance) return null;
  return <BillingBalanceButton balance={balance} provider={props.provider} busy={isFetching || opening} disabled={opening}
    onOpen={(scopeSlug) => openPage(() => props.api.openTopUp(props.provider, scopeSlug))} />;
}

/** Changing account or credential must never display the previous account's balance. */
export function AccountTools(props: Props) {
  const cacheKey = balanceCacheKey(props);
  return <AccountDetailsView key={cacheKey} {...props} />;
}

function AccountDetailsView({ api, provider, target, scope, credentialRef, images, onSignIn, disabled = false }: Props) {
  const { data: balance, isFetching: busy, refetch } = useAccountBalance({ api, provider, target, credentialRef });
  const { opening, openPage } = useAccountPage("Could not open account", refetch, disabled);
  const organizationSlug = balance?.scope.organizationSlug ?? scope?.organizationSlug;
  const manage = distributionCapabilities.accountPortalLinks && provider === "redpill" && organizationSlug ? () => void openPage(() => api.openOrganization(organizationSlug)) : undefined;
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
        {balance && <BillingBalanceButton balance={balance} provider={provider} busy={busy || opening} disabled={disabled || opening}
          onOpen={(scopeSlug) => openPage(() => api.openTopUp(provider, scopeSlug))} />}
        <AccountActions disabled={disabled || opening} onManage={manage} onSignIn={onSignIn} />
      </ItemActions>
    </Item>
  </div>;
}

function BillingBalanceButton({ balance, provider, busy, disabled = false, onOpen }: {
  balance: AccountBalance;
  provider: ServiceProvider;
  busy: boolean;
  disabled?: boolean;
  onOpen(scopeSlug: string): void;
}) {
  const scopeSlug = provider === "phala" ? balance.scope.workspaceSlug : balance.scope.organizationSlug;
  const amount = currency(Number(balance.balanceUsd));
  const canOpen = balance.canTopUp && Boolean(scopeSlug);
  return <Button type="button" variant="outline" size="sm" className="tabular-nums"
    aria-label={canOpen ? `Current balance: ${amount}. Open billing` : `Current balance: ${amount}`}
    aria-busy={busy} disabled={disabled || !canOpen}
    title={balance.grantedUsd != null && Number(balance.grantedUsd) > 0 ? `${currency(Number(balance.grantedUsd))} promo credits` : undefined}
    onClick={() => { if (scopeSlug) onOpen(scopeSlug); }}>{amount}</Button>;
}

function useAccountPage(title: string, refetch: () => Promise<unknown>, disabled = false) {
  const [opening, setOpening] = useState(false);
  const openingRef = useRef(false);
  const returningFromAccountPage = useRef(false);
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
    catch (error) { toastError(title, error); returningFromAccountPage.current = false; }
    finally { openingRef.current = false; setOpening(false); }
  }, [disabled, title]);
  return { opening, openPage };
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
  return <DropdownMenu>
    <DropdownMenuTrigger render={<Button type="button" size="icon-sm" variant="ghost" aria-label="Account actions" disabled={disabled} />}><Ellipsis aria-hidden /></DropdownMenuTrigger>
    <DropdownMenuContent align="end">
      {onManage && <DropdownMenuItem onClick={onManage}><ExternalLink aria-hidden />Manage</DropdownMenuItem>}
      {onSignIn && <DropdownMenuItem onClick={onSignIn}><ArrowLeftRight aria-hidden />Switch</DropdownMenuItem>}
    </DropdownMenuContent>
  </DropdownMenu>;
}
