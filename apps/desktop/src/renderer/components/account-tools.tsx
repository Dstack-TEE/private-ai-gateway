import { useState } from "react";
import { CircleDollarSign, ExternalLink } from "lucide-react";
import type { AccountBalance, AccountBalanceTarget, AccountScope, DesktopApi, ServiceProvider } from "../../shared/contracts";
import { errorMessage } from "../lib/error-message";
import { currency } from "../lib/usage-presentation";
import { Button } from "./ui/button";
import { FieldDescription, FieldError } from "./ui/field";

export function AccountTools({ api, provider, target, scope, disabled }: {
  api: Pick<DesktopApi, "getAccountBalance" | "openTopUp">;
  provider: ServiceProvider;
  target: AccountBalanceTarget;
  scope?: AccountScope;
  disabled: boolean;
}) {
  const [balance, setBalance] = useState<AccountBalance>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const run = async (action: () => Promise<void>) => {
    if (busy || disabled) return;
    setBusy(true);
    setError(undefined);
    try { await action(); } catch (error) { setError(errorMessage(error)); } finally { setBusy(false); }
  };
  const billingScope = balance?.scope ?? scope;
  const owner = billingScope?.organization ?? billingScope?.workspace;
  return <div className="space-y-2">
    <div className="flex flex-wrap items-center gap-2">
      <Button type="button" variant="outline" disabled={disabled || busy} onClick={() => void run(async () => { setBalance(undefined); setBalance(await api.getAccountBalance(target)); })}>
        <CircleDollarSign aria-hidden />{busy ? "Loading…" : balance ? "Refresh balance" : "Check balance"}
      </Button>
      <Button type="button" variant="link" disabled={disabled || busy} onClick={() => void run(() => api.openTopUp(provider))}>Top up<ExternalLink aria-hidden /></Button>
    </div>
    <FieldError>{error}</FieldError>
    {balance && <div role="status" className="text-sm tabular-nums">
      <p>{provider === "redpill" ? "Organization balance" : "Workspace balance"}{owner ? ` · ${owner}` : ""}: <strong>{currency(Number(balance.balanceUsd))} USD</strong></p>
      {balance.grantedUsd !== null && <p>Promotional credits: {currency(Number(balance.grantedUsd))} USD</p>}
    </div>}
    <FieldDescription>{provider === "redpill" ? "Balance is shared by the organization; workspace and key limits still apply. " : ""}{owner ? `On the billing website, select ${owner} before topping up.` : "Top up opens the official billing website."}</FieldDescription>
  </div>;
}
