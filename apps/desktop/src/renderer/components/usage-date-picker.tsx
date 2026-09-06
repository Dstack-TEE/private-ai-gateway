import { useState } from "react";
import { CalendarRange } from "lucide-react";
import type { DateRange } from "react-day-picker";
import { subMonths } from "date-fns";
import { useIsMobile } from "../hooks/use-mobile";
import { Calendar } from "./ui/calendar";
import { Button } from "./ui/button";
import { NativeSelect } from "./ui/native-select";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import { DATE_PRESETS, usageDateBounds, usageDateLabel, type UsageDateSelection } from "../lib/usage-dates";

export function UsageDatePicker({ value, onChange }: { value: UsageDateSelection; onChange(value: UsageDateSelection): void }) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState<DateRange>();
  const isMobile = useIsMobile();
  const label = usageDateLabel(value);
  const timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return <Popover open={open} onOpenChange={(next) => {
    if (next) {
      const bounds = usageDateBounds(value);
      setDraft({ from: bounds.start, to: bounds.end });
    }
    setOpen(next);
  }}>
    <PopoverTrigger render={<Button variant="outline" className="w-full justify-start font-normal" aria-label={`Date range: ${label}`} />}>
      <CalendarRange aria-hidden="true" /><span className="truncate">{label}</span>
    </PopoverTrigger>
    <PopoverContent align="end" className="w-auto max-w-[var(--available-width)] max-h-[var(--available-height)] gap-0 overflow-y-auto p-0" aria-label="Choose date range">
      <div className="border-b p-3">
        <NativeSelect className="w-full" aria-label="Quick date range" value={value.preset} onChange={(event) => {
          const preset = event.target.value;
          if (preset === "24h" || preset === "7d" || preset === "30d" || preset === "90d" || preset === "all") {
            onChange({ preset });
            setOpen(false);
          }
        }}>
          {Object.entries(DATE_PRESETS).map(([key, name]) => <option key={key} value={key}>{name}</option>)}
          <option value="custom" disabled>Custom range</option>
        </NativeSelect>
      </div>
      <Calendar mode="range" selected={draft} onSelect={setDraft} numberOfMonths={isMobile ? 1 : 2} showOutsideDays={isMobile} defaultMonth={isMobile ? draft?.from ?? new Date() : subMonths(draft?.to ?? new Date(), 1)} startMonth={new Date(1970, 0)} endMonth={new Date()} disabled={[{ before: new Date(1970, 0) }, { after: new Date() }]} captionLayout="dropdown" />
      <div className="flex flex-wrap items-center justify-between gap-3 border-t p-3">
        <span className="text-xs text-muted-foreground">{timeZone}</span>
        <div className="flex gap-2">
          <Button variant="ghost" size="sm" onClick={() => setOpen(false)}>Cancel</Button>
          <Button size="sm" disabled={!draft?.from || !draft.to} onClick={() => {
            if (!draft?.from || !draft.to) return;
            onChange({ preset: "custom", from: draft.from, to: draft.to });
            setOpen(false);
          }}>Apply</Button>
        </div>
      </div>
    </PopoverContent>
  </Popover>;
}
