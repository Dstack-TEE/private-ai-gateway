import { useQuery } from "@tanstack/react-query";
import type { DesktopApi, ListenConfig } from "../../shared/contracts";
import { localAddressKind } from "../lib/local-api-config";
import { Combobox, ComboboxContent, ComboboxEmpty, ComboboxInput, ComboboxItem, ComboboxList } from "./ui/combobox";
import { NetworkWarning } from "./network-warning";
import { Field, FieldError, FieldLabel } from "./ui/field";
import { Input } from "./ui/input";
import { FormField } from "./settings";

/** Listen address, port and client host shared by the Local API and web UI settings. */
export function ListenerFields({ api, idPrefix, value, minPort, access, clientHostNote, disabled, onChange }: {
  api: Pick<DesktopApi, "listListenAddresses">;
  idPrefix: string;
  value: ListenConfig;
  minPort: number;
  /** Completes the network warning: "Requests use unencrypted HTTP and …". */
  access?: string;
  clientHostNote: string;
  disabled: boolean;
  onChange(value: ListenConfig): void;
}) {
  const addressKind = localAddressKind(value.listenAddress);
  const update = <Key extends keyof ListenConfig>(key: Key, next: ListenConfig[Key]) => onChange({ ...value, [key]: next });
  return <>
    <div className="grid grid-cols-[minmax(0,1fr)_7rem] items-start gap-4">
      <Field>
        <div className="flex min-h-5 items-center gap-2"><FieldLabel htmlFor={`${idPrefix}-listen-address`}>Listen address</FieldLabel>{addressKind && addressKind !== "loopback" && <NetworkWarning access={access} />}</div>
        <ListenAddress api={api} id={`${idPrefix}-listen-address`} value={value.listenAddress} disabled={disabled} onChange={(next) => update("listenAddress", next)} />
      </Field>
      <Field>
        <FieldLabel className="min-h-5" htmlFor={`${idPrefix}-port`}>Port</FieldLabel>
        <Input id={`${idPrefix}-port`} type="number" min={minPort} max="65535" required autoComplete="off" value={value.port} disabled={disabled} onChange={(event) => update("port", Number(event.target.value))} />
      </Field>
    </div>
    <FormField id={`${idPrefix}-client-host`} label="Client host" description={addressKind === "unspecified" ? "Required for all-interface listeners. Use an address reachable by your clients." : clientHostNote}>
      <Input id={`${idPrefix}-client-host`} aria-describedby={`${idPrefix}-client-host-note`} value={value.clientHost ?? ""} required={addressKind === "unspecified"} placeholder="Same as listen address" disabled={disabled} spellCheck={false} autoComplete="off" onChange={(event) => update("clientHost", event.target.value || undefined)} />
    </FormField>
  </>;
}

export function ListenAddress({ api, id, value, disabled, onChange }: {
  api: Pick<DesktopApi, "listListenAddresses">;
  id: string;
  value: string;
  disabled: boolean;
  onChange(value: string): void;
}) {
  const { data: addresses = [], error: addressError } = useQuery({
    queryKey: ["listen-addresses"], queryFn: () => api.listListenAddresses(), staleTime: 0,
  });
  const error = addressError ? "Network interfaces unavailable. Enter an IP address manually." : undefined;
  const options = [...new Set(["127.0.0.1", "::1", ...addresses.map((item) => item.address), "0.0.0.0", "::"])];
  return <>
    <Combobox items={options} inputValue={value} value={value}
      // Escape with the list closed would clear the address; let it close the surrounding dialog.
      onInputValueChange={(next, details) => { if (details.reason === "escape-key") details.allowPropagation(); else onChange(next); }}
      onValueChange={(next) => { if (next) onChange(next); }}>
      <ComboboxInput id={id} aria-label="Listen address" triggerLabel="Choose listen address" required disabled={disabled} autoComplete="off" spellCheck={false} />
      <ComboboxContent>
        <ComboboxEmpty>No matching address. You can enter an IP address.</ComboboxEmpty>
        <ComboboxList>{(address: string) => <ComboboxItem key={address} value={address}>
          <span>{address}</span><span className="ml-auto truncate text-muted-foreground">{address === "0.0.0.0" || address === "::" ? "All interfaces" : addresses.find((item) => item.address === address)?.name}</span>
        </ComboboxItem>}</ComboboxList>
      </ComboboxContent>
    </Combobox>
    <FieldError>{error}</FieldError>
  </>;
}
