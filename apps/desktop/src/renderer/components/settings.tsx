import { useId, type ComponentProps, type PropsWithChildren, type ReactNode } from "react";
import { ChevronRight, ExternalLink } from "lucide-react";
import { SwitchControl } from "./controls";
import { Field, FieldLabel, FieldDescription } from "./ui/field";
import { Item, ItemContent, ItemTitle, ItemDescription, ItemActions, ItemGroup } from "./ui/item";

/** A titled group of rows; `detail` is a short summary beside the title. */
export function SettingsSection({ title, detail, children }: PropsWithChildren<{ title: string; detail?: ReactNode }>): React.JSX.Element {
  const id = useId();
  return <section className="grid gap-2" aria-labelledby={id}>
    <h2 className="flex items-center gap-2 font-semibold" id={id}>{title}{detail && <span className="ml-auto truncate text-xs font-normal text-muted-foreground">{detail}</span>}</h2>
    <ItemGroup>{children}</ItemGroup>
  </section>;
}

/** A row that opens a dialog, a page or a link; `title` names it and `description` describes it. */
export function SettingsLink({ title, description, external = false, ...props }: Omit<ComponentProps<"button">, "title" | "children" | "className"> & { title: string; description?: ReactNode; external?: boolean }): React.JSX.Element {
  const Icon = external ? ExternalLink : ChevronRight;
  const titleId = useId();
  const descriptionId = useId();
  return <Item variant="outline" render={<button type="button" aria-labelledby={titleId} aria-describedby={description ? descriptionId : undefined} {...props} />}>
    <ItemContent><ItemTitle id={titleId}>{title}</ItemTitle>{description && <ItemDescription id={descriptionId}>{description}</ItemDescription>}</ItemContent>
    <ItemActions><Icon className="size-4 text-muted-foreground" aria-hidden="true" /></ItemActions>
  </Item>;
}

export function SettingsToggle({ label, description, ...props }: ComponentProps<typeof SwitchControl> & { description?: ReactNode }): React.JSX.Element {
  const descriptionId = useId();
  const controlId = useId();
  return <Item variant="outline">
    <ItemContent><ItemTitle><FieldLabel htmlFor={controlId}>{label}</FieldLabel></ItemTitle>{description && <ItemDescription id={descriptionId}>{description}</ItemDescription>}</ItemContent>
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
