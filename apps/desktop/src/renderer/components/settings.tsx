import { Children, Fragment, isValidElement, useId, type ComponentProps, type PropsWithChildren, type ReactNode } from "react";
import { ChevronRight, ExternalLink } from "lucide-react";
import { SwitchControl } from "./controls";
import { Field, FieldLabel, FieldDescription } from "./ui/field";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions, ItemGroup } from "./ui/item";
import { Separator } from "./ui/separator";
import { ActionItem } from "./action-item";

export function SettingsList({ children }: PropsWithChildren): React.JSX.Element {
  return <ItemGroup className="gap-0 has-data-[size=sm]:gap-0 has-data-[size=xs]:gap-0 overflow-hidden rounded-2xl border bg-card text-card-foreground [&>[data-slot=item]]:min-h-13 [&>[data-slot=item]]:border-0 [&>[data-slot=item]]:py-2.5">
    {Children.toArray(children).map((child, index) => <Fragment key={isValidElement(child) ? child.key : index}>{index > 0 && <Separator />}{child}</Fragment>)}
  </ItemGroup>;
}

/** A titled group of rows; `detail` is a short summary beside the title. */
export function SettingsSection({ title, detail, children }: PropsWithChildren<{ title: string; detail?: ReactNode }>): React.JSX.Element {
  const id = useId();
  return <section className="group mt-5 [&:first-child]:mt-0" aria-labelledby={id}>
    <h2 className="mx-0.5 mb-2 flex min-h-5 items-center gap-2 text-sm font-semibold" id={id}>{title}{detail && <span className="ml-auto truncate text-xs font-normal text-muted-foreground">{detail}</span>}</h2>
    <SettingsList>{children}</SettingsList>
  </section>;
}

export function SettingsLink({ title, description, external = false, ...props }: Omit<ComponentProps<typeof ActionItem>, "title" | "children" | "className"> & { title: string; description?: ReactNode; external?: boolean }): React.JSX.Element {
  const Icon = external ? ExternalLink : ChevronRight;
  return <ActionItem {...props}>
    <ItemContent><ItemTitle>{title}</ItemTitle>{description && <ItemDescription>{description}</ItemDescription>}</ItemContent>
    <ItemActions><Icon className="size-4 text-muted-foreground" aria-hidden="true" /></ItemActions>
  </ActionItem>;
}

export function SettingsToggle({ label, description, variant = "default", ...props }: ComponentProps<typeof SwitchControl> & { description?: ReactNode; variant?: ComponentProps<typeof Item>["variant"] }): React.JSX.Element {
  const descriptionId = useId();
  const controlId = useId();
  return <Item variant={variant}>
    <ItemContent className="min-w-0"><ItemTitle><FieldLabel htmlFor={controlId}>{label}</FieldLabel></ItemTitle>{description && <ItemDescription id={descriptionId} className="line-clamp-none">{description}</ItemDescription>}</ItemContent>
    <ItemActions><SwitchControl id={controlId} label={label} aria-describedby={description ? descriptionId : undefined} {...props} /></ItemActions>
  </Item>;
}

export function FormField({ id, label, description, children }: PropsWithChildren<{ id: string; label: string; description?: ReactNode }>): React.JSX.Element {
  return <Field>
    <FieldLabel htmlFor={id}>{label}</FieldLabel>
    {children}
    {description && <FieldDescription id={`${id}-note`}>{description}</FieldDescription>}
  </Field>;
}
