import { useEffect, useRef, useState } from "react";
import ipaddr from "ipaddr.js";
import type { DesktopApi } from "../../shared/contracts";
import { Combobox, ComboboxContent, ComboboxEmpty, ComboboxInput, ComboboxItem, ComboboxList } from "./ui/combobox";
import { FieldDescription } from "./ui/field";

export function localAddressKind(value: string) {
  const address = value.trim();
  return ipaddr.isValid(address) ? ipaddr.parse(address).range() : undefined;
}

export function ListenAddress({ api, value, disabled, onChange }: {
  api: Pick<DesktopApi, "listListenAddresses">;
  value: string;
  disabled: boolean;
  onChange(value: string): void;
}) {
  const [addresses, setAddresses] = useState<Awaited<ReturnType<DesktopApi["listListenAddresses"]>>>([]);
  const [error, setError] = useState<string>();
  const [container, setContainer] = useState<HTMLDialogElement | null>(null);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    let current = true;
    void api.listListenAddresses().then((items) => { if (current) setAddresses(items); })
      .catch(() => { if (current) setError("Network interfaces unavailable. Enter an IP address manually."); });
    return () => { current = false; };
  }, [api]);
  const options = [...new Set(["127.0.0.1", "::1", ...addresses.map((item) => item.address), "0.0.0.0", "::"])];
  return <>
    <Combobox items={options} inputValue={value} onInputValueChange={onChange} value={value}
      onValueChange={(next) => { if (next) onChange(next); }}
      onOpenChange={(open) => { if (open) setContainer(input.current?.closest("dialog") ?? null); }}>
      <ComboboxInput ref={input} id="local-listen-address" aria-label="Listen address" triggerLabel="Choose listen address" required disabled={disabled} autoComplete="off" spellCheck={false} aria-describedby={error ? "listen-address-error" : undefined} />
      <ComboboxContent container={container}>
        <ComboboxEmpty>No matching address. You can enter an IP address.</ComboboxEmpty>
        <ComboboxList>{(address: string) => <ComboboxItem key={address} value={address}>
          <span>{address}</span><span className="ml-auto truncate text-muted-foreground">{address === "0.0.0.0" || address === "::" ? "All interfaces" : addresses.find((item) => item.address === address)?.name}</span>
        </ComboboxItem>}</ComboboxList>
      </ComboboxContent>
    </Combobox>
    {error && <FieldDescription id="listen-address-error" role="status">{error}</FieldDescription>}
  </>;
}
