import { useCallback, useEffect, useRef, useState } from "react";
import { Building2, Ellipsis, ExternalLink, LogIn, RefreshCw, UserRound } from "lucide-react";
import type { AccountBalance, AccountBalanceTarget, AccountScope, DesktopApi, ServiceProvider } from "../../shared/contracts";
import { errorMessage } from "../lib/error-message";
import { currency } from "../lib/usage-presentation";
import { Button } from "./ui/button";
import { FieldError } from "./ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "./ui/item";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "./ui/dropdown-menu";

type Props = {
  api: Pick<DesktopApi, "getAccountBalance" | "openTopUp">;
  provider: ServiceProvider;
  target: AccountBalanceTarget;
  scope?: AccountScope;
  credentialRef?: string;
  accountName?: string | null;
  onSignIn?(): void;
  disabled?: boolean;
  compact?: boolean;
};

/** Changing account or credential must never display the previous account's balance. */
export function AccountTools(props: Props) {
  const id = props.target.kind === "login" ? props.target.id : props.target.profileId;
  return <AccountBalanceView key={`${props.provider}:${props.target.kind}:${id}:${props.credentialRef ?? ""}`} {...props} />;
}

function AccountBalanceView({ api, provider, target, scope, accountName, onSignIn, disabled = false, compact = false }: Props) {
  const [balance, setBalance] = useState<AccountBalance>();
  const [error, setError] = useState<string>();
  const [linkError, setLinkError] = useState<string>();
  const [busy, setBusy] = useState(true);
  const [opening, setOpening] = useState(false);
  const refresh = useRef<() => void>(() => {});
  const returningFromTopUp = useRef(false);
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
      returningFromTopUp.current = false;
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
    const onReturn = () => { void load(returningFromTopUp.current); };
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

  const topUp = useCallback(async () => {
    if (openingRef.current || disabled) return;
    openingRef.current = true;
    setOpening(true);
    setLinkError(undefined);
    returningFromTopUp.current = true;
    try { await api.openTopUp(provider); }
    catch (error) { setLinkError(errorMessage(error)); returningFromTopUp.current = false; }
    finally { openingRef.current = false; setOpening(false); }
  }, [api, provider, disabled]);
  const owner = (balance?.scope ?? scope)?.organization ?? (balance?.scope ?? scope)?.workspace;
  const amount = balance ? currency(Number(balance.balanceUsd)) : busy ? "…" : "Unavailable";
  if (compact) return <Button type="button" variant="outline" size="sm" className="tabular-nums"
    aria-label={`Current balance: ${amount}`} title={error ?? `${owner ?? "Account"} · USD · Refresh balance`}
    disabled={busy} onClick={() => refresh.current()}>{amount}</Button>;
  const organization = (balance?.scope ?? scope)?.organization;
  const name = organization ?? accountName ?? owner ?? "Account";
  return <div aria-label="Account balance">
    <Item variant="outline" size="sm">
      <ItemMedia variant="icon">{organization ? <Building2 aria-hidden /> : <UserRound aria-hidden />}</ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle className="line-clamp-none wrap-anywhere">{name}</ItemTitle>
        <ItemDescription className="line-clamp-none" role="status" aria-live="polite" aria-busy={busy}>
          Balance: <span className="tabular-nums" aria-label="Balance in USD">{balance ? currency(Number(balance.balanceUsd)) : busy ? "Loading…" : "Unavailable"}</span>
          {balance?.grantedUsd != null && Number(balance.grantedUsd) > 0 && <> · {currency(Number(balance.grantedUsd))} promo credits</>}
        </ItemDescription>
      </ItemContent>
      <ItemActions>
        <Button type="button" size="sm" variant="outline" title={`Top up ${name}`} disabled={disabled || opening} onClick={() => void topUp()}>Top up<ExternalLink aria-hidden /></Button>
        <AccountActions disabled={disabled} refreshing={busy} onRefresh={() => refresh.current()} onSignIn={onSignIn} />
      </ItemActions>
    </Item>
    <FieldError>{linkError ?? error}</FieldError>
  </div>;
}

function AccountActions({ disabled, refreshing, onRefresh, onSignIn }: {
  disabled: boolean;
  refreshing: boolean;
  onRefresh(): void;
  onSignIn?(): void;
}) {
  const trigger = useRef<HTMLButtonElement>(null);
  const [container, setContainer] = useState<HTMLDialogElement | null>(null);
  return <DropdownMenu onOpenChange={(open) => { if (open) setContainer(trigger.current?.closest("dialog") ?? null); }}>
    <DropdownMenuTrigger render={<Button ref={trigger} type="button" size="icon-sm" variant="ghost" aria-label="Account actions" disabled={disabled} />}><Ellipsis aria-hidden /></DropdownMenuTrigger>
    <DropdownMenuContent align="end" container={container ?? undefined}>
      <DropdownMenuItem disabled={refreshing} onClick={onRefresh}><RefreshCw aria-hidden />Refresh balance</DropdownMenuItem>
      {onSignIn && <DropdownMenuItem onClick={onSignIn}><LogIn aria-hidden />Sign in again</DropdownMenuItem>}
    </DropdownMenuContent>
  </DropdownMenu>;
}
