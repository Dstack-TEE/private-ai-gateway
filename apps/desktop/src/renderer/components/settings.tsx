import { useId, type ComponentProps, type PropsWithChildren, type ReactNode } from "react";
import { ChevronRight, ExternalLink } from "lucide-react";
import { SwitchControl } from "./controls";
import { Button } from "./ui/button";
import { Field, FieldLabel, FieldDescription, FieldContent, FieldGroup } from "./ui/field";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions } from "./ui/item";

export function SettingsSection({ title, children }: PropsWithChildren<{ title: string }>): React.JSX.Element {
  const id = useId();
  return <section className="group" aria-labelledby={id}>
    <h2 className="group-title" id={id}>{title}</h2>
    <FieldGroup>{children}</FieldGroup>
  </section>;
}

export function RowContent({ title, description, descriptionId }: { title: ReactNode; description?: ReactNode; descriptionId?: string }): React.JSX.Element {
  return <span className="row-main"><span className="row-title">{title}</span>{description && <span className="row-note" id={descriptionId}>{description}</span>}</span>;
}

export function SettingsLink({ title, description, external = false, ...props }: Omit<ComponentProps<typeof Button>, "title" | "children" | "className"> & { title: string; description?: ReactNode; external?: boolean }): React.JSX.Element {
  const Icon = external ? ExternalLink : ChevronRight;
  return <Item render={<Button type="button" variant="ghost" className="h-auto justify-start whitespace-normal" {...props} />}>
    <ItemContent><ItemTitle>{title}</ItemTitle>{description && <ItemDescription>{description}</ItemDescription>}</ItemContent>
    <ItemActions><Icon aria-hidden="true" /></ItemActions>
  </Item>;
}

export function SettingsToggle({ label, description, ...props }: ComponentProps<typeof SwitchControl> & { description?: ReactNode }): React.JSX.Element {
  const descriptionId = useId();
  const controlId = useId();
  return <Field orientation="horizontal">
    <FieldContent><FieldLabel htmlFor={controlId}>{label}</FieldLabel>{description && <FieldDescription id={descriptionId}>{description}</FieldDescription>}</FieldContent>
    <SwitchControl id={controlId} label={label} aria-describedby={description ? descriptionId : undefined} {...props} />
  </Field>;
}

export function FormField({ id, label, description, children }: PropsWithChildren<{ id: string; label: string; description?: ReactNode }>): React.JSX.Element {
  return <Field>
    <FieldLabel htmlFor={id}>{label}</FieldLabel>
    {children}
    {description && <FieldDescription id={`${id}-note`}>{description}</FieldDescription>}
  </Field>;
}
