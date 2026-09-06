import { Children, Fragment, isValidElement, useId, type ComponentProps, type PropsWithChildren, type ReactNode } from "react";
import { ChevronRight, ExternalLink } from "lucide-react";
import { SwitchControl } from "./controls";
import { Field, FieldLabel, FieldDescription } from "./ui/field";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions, ItemGroup } from "./ui/item";
import { Separator } from "./ui/separator";
import { ActionItem } from "./action-item";

export function SettingsList({ children }: PropsWithChildren): React.JSX.Element {
  return <ItemGroup className="gap-0 overflow-hidden rounded-2xl border">
    {Children.toArray(children).map((child, index) => <Fragment key={isValidElement(child) ? child.key : index}>{index > 0 && <Separator />}{child}</Fragment>)}
  </ItemGroup>;
}

export function SettingsSection({ title, children }: PropsWithChildren<{ title: string }>): React.JSX.Element {
  const id = useId();
  return <section className="group" aria-labelledby={id}>
    <h2 className="group-title" id={id}>{title}</h2>
    <SettingsList>{children}</SettingsList>
  </section>;
}

export function RowContent({ title, description, descriptionId }: { title: ReactNode; description?: ReactNode; descriptionId?: string }): React.JSX.Element {
  return <span className="row-main"><span className="row-title">{title}</span>{description && <span className="row-note" id={descriptionId}>{description}</span>}</span>;
}

export function SettingsLink({ title, description, external = false, ...props }: Omit<ComponentProps<typeof ActionItem>, "title" | "children" | "className"> & { title: string; description?: ReactNode; external?: boolean }): React.JSX.Element {
  const Icon = external ? ExternalLink : ChevronRight;
  return <ActionItem {...props}>
    <ItemContent><ItemTitle>{title}</ItemTitle>{description && <ItemDescription>{description}</ItemDescription>}</ItemContent>
    <ItemActions><Icon aria-hidden="true" /></ItemActions>
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
