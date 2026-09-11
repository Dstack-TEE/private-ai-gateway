import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeftRight, Ellipsis, ExternalLink, RefreshCw } from "lucide-react";
import type { AccountBalance, AccountBalanceTarget, AccountImages, AccountScope, DesktopApi, ServiceProvider } from "../../shared/contracts";
import { errorMessage } from "../lib/error-message";
import { currency } from "../lib/usage-presentation";
import { Avatar, AvatarFallback, AvatarImage } from "./ui/avatar";
import { Button } from "./ui/button";
import { FieldError } from "./ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemTitle } from "./ui/item";
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
  return <AccountDetailsView key={`${props.provider}:${props.target.kind}:${id}:${props.credentialRef ?? ""}`} {...props} />;
}

function AccountDetailsView({ api, provider, target, scope, images, onSignIn, disabled = false, compact = false }: Props) {
  const [balance, setBalance] = useState<AccountBalance | null>();
  const [error, setError] = useState<string>();
  const [linkError, setLinkError] = useState<string>();
  const [busy, setBusy] = useState(true);
  const [opening, setOpening] = useState(false);
  const refresh = useRef<() => void>(() => {});
  const returningFromAccountPage = useRef(false);
  const openingRef = useRef(false);
  const kind = target.kind;
  const id = target.kind === "login" ? target.id : target.profileId;

  useEffect(() => {
    let disposed = false;
    let inFlight = false;
    let lastAttempt = 0;
    const load = async (force = false) => {
      if (disposed || inFlight || document.visibilityState === "hidden" || (!force && Date.now() - lastAttempt < 30_000)) return;
      inFlight = true;
      lastAttempt = Date.now();
      returningFromAccountPage.current = false;
      setBusy(true);
      try {
        const value = await api.getAccountBalance(kind === "login" ? { kind, id } : { kind, profileId: id });
        if (!disposed) { setBalance(value); setError(undefined); }
      } catch (error) {
        if (!disposed) { setBalance(undefined); setError(errorMessage(error)); }
      } finally {
        inFlight = false;
        if (!disposed) setBusy(false);
      }
    };
    refresh.current = () => { void load(true); };
    const onReturn = () => { void load(returningFromAccountPage.current); };
    window.addEventListener("focus", onReturn);
    document.addEventListener("visibilitychange", onReturn);
    const timer = setInterval(() => void load(), compact ? 300_000 : 60_000);
    void load();
    return () => {
      disposed = true;
      refresh.current = () => {};
      clearInterval(timer);
      window.removeEventListener("focus", onReturn);
      document.removeEventListener("visibilitychange", onReturn);
    };
  }, [api, kind, id, compact]);

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
  const organizationId = balance?.organizationId ?? scope?.organizationId;
  const topUp = () => balance && openPage(() => api.openTopUp(provider, organizationId ?? undefined));
  const manage = provider === "redpill" && organizationId ? () => void openPage(() => api.openOrganization(organizationId)) : undefined;
  const displayScope = provider === "redpill" && !compact ? scope ?? balance?.scope : balance?.scope ?? scope;
  const owner = displayScope?.organization ?? displayScope?.workspace;
  const amount = balance ? currency(Number(balance.balanceUsd)) : busy ? "…" : "Unavailable";
  if (compact && balance === null) return null;
  if (compact) return <Button type="button" variant="outline" size="sm" className="tabular-nums"
    aria-label={`Current balance: ${amount}`} title={linkError ?? error ?? `${owner ?? "Account"} · Open billing`}
    disabled={opening || !balance} onClick={() => void topUp()}>{amount}</Button>;
  const name = owner ?? "Account";
  return <div aria-label="Account details">
    <Item variant="outline" size="sm" className="grid grid-cols-[1.5rem_minmax(0,_1fr)_auto] gap-x-2 gap-y-0">
      <AccountAvatar name={name} src={images?.organization} />
      <ItemContent className="min-h-8 min-w-0 justify-center">
        <ItemTitle className="line-clamp-none wrap-anywhere">{name}</ItemTitle>
      </ItemContent>
      <ItemActions>
        {balance?.canTopUp && <Button type="button" size="sm" variant="outline" title={`Top up ${name}`} disabled={disabled || opening} onClick={() => void topUp()}>Top up<ExternalLink aria-hidden /></Button>}
        <AccountActions disabled={disabled || opening} refreshing={busy} onManage={manage} onRefresh={balance === null ? undefined : () => refresh.current()} onSignIn={onSignIn} />
      </ItemActions>
      {balance && <ItemDescription className="col-span-2 col-start-2 line-clamp-none" role="status" aria-live="polite" aria-busy={busy}>
        Balance: <span className="tabular-nums" aria-label="Balance in USD">{currency(Number(balance.balanceUsd))}</span>
        {balance.grantedUsd != null && Number(balance.grantedUsd) > 0 && <> · {currency(Number(balance.grantedUsd))} promo credits</>}
      </ItemDescription>}
    </Item>
    <FieldError>{linkError ?? error}</FieldError>
  </div>;
}

function AccountAvatar({ name, src }: { name: string; src?: string | null }) {
  const initials = name.trim().split(/\s+/).slice(0, 2).map((part) => part.charAt(0)).join("").toUpperCase();
  return <Avatar size="sm">
    {src && <AvatarImage src={src} alt={`${name} avatar`} referrerPolicy="no-referrer" />}
    <AvatarFallback>{initials}</AvatarFallback>
  </Avatar>;
}

function AccountActions({ disabled, refreshing, onRefresh, onSignIn, onManage }: {
  disabled: boolean;
  refreshing: boolean;
  onRefresh?(): void;
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
      {onRefresh && <DropdownMenuItem disabled={refreshing} onClick={onRefresh}><RefreshCw aria-hidden />Refresh balance</DropdownMenuItem>}
    </DropdownMenuContent>
  </DropdownMenu>;
}
