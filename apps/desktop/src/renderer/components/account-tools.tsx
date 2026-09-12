import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeftRight, Ellipsis, ExternalLink } from "lucide-react";
import type { AccountBalance, AccountBalanceTarget, AccountImages, AccountScope, DesktopApi, ServiceProvider } from "../../shared/contracts";
import { useQuery } from "@tanstack/react-query";
import { errorMessage } from "../lib/error-message";
import { currency } from "../lib/usage-presentation";
import { Avatar, AvatarFallback, AvatarImage } from "./ui/avatar";
import { Button } from "./ui/button";
import { FieldError } from "./ui/field";
import { Item, ItemActions, ItemContent, ItemTitle } from "./ui/item";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "./ui/dropdown-menu";

type Props = {
  api: Pick<DesktopApi, "getAccountBalance" | "openTopUp" | "openOrganization">;
  provider: ServiceProvider;
  target: AccountBalanceTarget;
  scope?: AccountScope;
  credentialRef?: string;
  images?: AccountImages;
  onSignIn?(): void;
  disabled?: boolean;
  compact?: boolean;
};

/** Changing account or credential must never display the previous account's balance. */
export function AccountTools(props: Props) {
  const id = props.target.kind === "login" ? props.target.id : props.target.profileId;
  const cacheKey = `${props.provider}:${props.target.kind}:${id}:${props.credentialRef ?? ""}`;
  return <AccountDetailsView key={cacheKey} cacheKey={cacheKey} {...props} />;
}

function AccountDetailsView({ cacheKey, api, provider, target, scope, images, onSignIn, disabled = false, compact = false }: Props & { cacheKey: string }) {
  const [linkError, setLinkError] = useState<string>();
  const [opening, setOpening] = useState(false);
  const openingRef = useRef(false);
  const returningFromAccountPage = useRef(false);
  const { data: balance, isFetching: busy, refetch } = useQuery({
    queryKey: ["account-balance", cacheKey], queryFn: () => api.getAccountBalance(target),
    refetchInterval: compact ? 300_000 : 60_000, staleTime: 30_000, retry: false,
  });
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
    setLinkError(undefined);
    returningFromAccountPage.current = true;
    try { await action(); }
    catch (error) { setLinkError(errorMessage(error)); returningFromAccountPage.current = false; }
    finally { openingRef.current = false; setOpening(false); }
  }, [disabled]);
  const organizationSlug = balance?.scope.organizationSlug ?? scope?.organizationSlug;
  const billingSlug = provider === "phala" ? balance?.scope.workspaceSlug ?? scope?.workspaceSlug : organizationSlug;
  const canOpenBilling = Boolean(billingSlug);
  const openBilling = () => balance && canOpenBilling && openPage(() => api.openTopUp(provider, billingSlug ?? undefined));
  const manage = provider === "redpill" && organizationSlug ? () => void openPage(() => api.openOrganization(organizationSlug)) : undefined;
  const displayScope = provider === "redpill" && !compact ? scope ?? balance?.scope : balance?.scope ?? scope;
  const owner = displayScope?.organization ?? displayScope?.workspace;
  if (compact && !balance) return null;
  const amount = balance ? currency(Number(balance.balanceUsd)) : "";
  if (compact) return <Button type="button" variant="outline" size="sm" className="tabular-nums"
    aria-label={`Current balance: ${amount}`} title={linkError ?? `${owner ?? "Account"}${canOpenBilling ? " · Open billing" : ""}`}
    disabled={opening || !balance || !canOpenBilling} onClick={() => void openBilling()}>{amount}</Button>;
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
    <FieldError>{linkError}</FieldError>
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
