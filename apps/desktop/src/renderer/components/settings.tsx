import { Children, Fragment, isValidElement, useId, type ComponentProps, type PropsWithChildren, type ReactNode } from "react";
import { ChevronRight, ExternalLink } from "lucide-react";
import { SwitchControl } from "./controls";
import { Field, FieldLabel, FieldDescription } from "./ui/field";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions, ItemGroup } from "./ui/item";
import { Separator } from "./ui/separator";
import { ActionItem } from "./action-item";
import { cn } from "../lib/utils";

/** A card of rows divided by separators; its rows are `SettingsItem`s. */
export function SettingsList({ children }: PropsWithChildren): React.JSX.Element {
  return <ItemGroup className="gap-0 has-data-[size=sm]:gap-0 has-data-[size=xs]:gap-0 overflow-hidden rounded-2xl border bg-card text-card-foreground">
    {Children.toArray(children).map((child, index) => <Fragment key={isValidElement(child) ? child.key : index}>{index > 0 && <Separator />}{child}</Fragment>)}
  </ItemGroup>;
}

/** A titled group of rows; `detail` is a short summary beside the title. */
export function SettingsSection({ title, detail, children }: PropsWithChildren<{ title: string; detail?: ReactNode }>): React.JSX.Element {
  const id = useId();
  return <section className="mt-5 first:mt-0" aria-labelledby={id}>
    <h2 className="mx-0.5 mb-2 flex min-h-5 items-center gap-2 text-sm font-semibold" id={id}>{title}{detail && <span className="ml-auto truncate text-xs font-normal text-muted-foreground">{detail}</span>}</h2>
    <SettingsList>{children}</SettingsList>
  </section>;
}

const SETTINGS_ITEM = "min-h-13 border-0 py-2.5";

/** A row of a `SettingsList`. */
export function SettingsItem({ className, ...props }: ComponentProps<typeof Item>): React.JSX.Element {
  return <Item className={cn(SETTINGS_ITEM, className)} {...props} />;
}

/** A row that opens a dialog, a page or a link; `title` names it and `description` describes it. */
export function SettingsLink({ title, description, external = false, ...props }: Omit<ComponentProps<typeof ActionItem>, "title" | "children" | "className"> & { title: string; description?: ReactNode; external?: boolean }): React.JSX.Element {
  const Icon = external ? ExternalLink : ChevronRight;
  const titleId = useId();
  const descriptionId = useId();
  return <ActionItem className={SETTINGS_ITEM} aria-labelledby={titleId} aria-describedby={description ? descriptionId : undefined} {...props}>
    <ItemContent><ItemTitle id={titleId}>{title}</ItemTitle>{description && <ItemDescription id={descriptionId}>{description}</ItemDescription>}</ItemContent>
    <ItemActions><Icon className="size-4 text-muted-foreground" aria-hidden="true" /></ItemActions>
  </ActionItem>;
}

/** A switch row. */
export function SettingsToggle({ label, description, variant = "default", ...props }: ComponentProps<typeof SwitchControl> & { description?: ReactNode; variant?: ComponentProps<typeof Item>["variant"] }): React.JSX.Element {
  const descriptionId = useId();
  const controlId = useId();
  return <SettingsItem variant={variant}>
    <ItemContent className="min-w-0"><ItemTitle><FieldLabel htmlFor={controlId}>{label}</FieldLabel></ItemTitle>{description && <ItemDescription id={descriptionId} className="line-clamp-none">{description}</ItemDescription>}</ItemContent>
    <ItemActions><SwitchControl id={controlId} label={label} aria-describedby={description ? descriptionId : undefined} {...props} /></ItemActions>
  </SettingsItem>;
}

export function FormField({ id, label, description, className, children }: PropsWithChildren<{ id: string; label: string; description?: ReactNode; className?: string }>): React.JSX.Element {
  return <Field className={className}>
    <FieldLabel htmlFor={id}>{label}</FieldLabel>
    {children}
    {description && <FieldDescription id={`${id}-note`}>{description}</FieldDescription>}
  </Field>;
}
