import { Radio } from "@base-ui/react/radio";
import { RadioGroup } from "@base-ui/react/radio-group";
import { cn } from "../../lib/utils";

function RadioGroupItem({ className, ...props }: Radio.Root.Props<string>) {
  return <Radio.Root data-slot="radio-group-item" className={cn(
    "inline-flex size-4 shrink-0 items-center justify-center rounded-full border border-input text-primary outline-none data-checked:border-primary focus-visible:ring-3 focus-visible:ring-ring/30 data-disabled:opacity-50",
    className,
  )} {...props}>
    <Radio.Indicator className="size-2 rounded-full bg-current" />
  </Radio.Root>;
}

export { RadioGroup, RadioGroupItem };
