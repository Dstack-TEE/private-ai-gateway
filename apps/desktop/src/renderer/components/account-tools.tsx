import { useCallback, useEffect, useRef, useState } from "react";
import { ExternalLink, RefreshCw } from "lucide-react";
import type { AccountBalance, AccountBalanceTarget, AccountScope, DesktopApi, ServiceProvider } from "../../shared/contracts";
import { errorMessage } from "../lib/error-message";
import { currency } from "../lib/usage-presentation";
import { Button } from "./ui/button";
import { FieldError } from "./ui/field";

type Props = {
  api: Pick<DesktopApi, "getAccountBalance" | "openTopUp">;
  provider: ServiceProvider;
  target: AccountBalanceTarget;
  scope?: AccountScope;
  credentialRef?: string;
  disabled?: boolean;
  compact?: boolean;
};

/** Changing account or credential must never display the previous account's balance. */
export function AccountTools(props: Props) {
  const id = props.target.kind === "login" ? props.target.id : props.target.profileId;
  return <AccountBalanceView key={`${props.provider}:${props.target.kind}:${id}:${props.credentialRef ?? ""}`} {...props} />;
}

function AccountBalanceView({ api, provider, target, scope, disabled = false, compact = false }: Props) {
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
  return <div className="w-full min-w-0 space-y-1.5" aria-label="Account balance">
    <div className="flex items-center justify-between gap-3" role="status" aria-live="polite" aria-busy={busy}>
      <span className="min-w-0 text-xs text-muted-foreground wrap-anywhere" title={provider === "redpill" ? "Shared organization balance" : "Workspace balance"}>Balance</span>
      <div className="flex shrink-0 items-center gap-1">
        <span className="text-sm font-medium tabular-nums" aria-label="Balance in USD">{balance ? currency(Number(balance.balanceUsd)) : busy ? "Loading…" : "Unavailable"}</span>
        <Button type="button" size="icon-xs" variant="ghost" aria-label="Refresh balance" disabled={disabled || busy} onClick={() => refresh.current()}><RefreshCw className={busy ? "animate-spin motion-reduce:animate-none" : ""} aria-hidden /></Button>
        <Button type="button" size="xs" variant="link" title={owner ? `Top up ${owner}` : "Open billing"} disabled={disabled || opening} onClick={() => void topUp()}>Top up<ExternalLink aria-hidden /></Button>
      </div>
    </div>
    {balance?.grantedUsd != null && Number(balance.grantedUsd) > 0 && <div className="flex items-center justify-between gap-3 text-xs text-muted-foreground tabular-nums"><span>Promo credits</span><span>{currency(Number(balance.grantedUsd))}</span></div>}
    <FieldError>{linkError ?? error}</FieldError>
  </div>;
}
