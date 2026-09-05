import { useId, type ComponentProps, type PropsWithChildren, type ReactNode } from "react";
import { ChevronRight, ExternalLink } from "lucide-react";
import { SwitchControl } from "./controls";
import { Button } from "./ui/button";

export function SettingsSection({ title, children }: PropsWithChildren<{ title: string }>): React.JSX.Element {
  const id = useId();
  return <section className="group" aria-labelledby={id}>
    <h2 className="group-title" id={id}>{title}</h2>
    <div className="inset">{children}</div>
  </section>;
}

export function RowContent({ title, description, descriptionId }: { title: ReactNode; description?: ReactNode; descriptionId?: string }): React.JSX.Element {
  return <span className="row-main"><span className="row-title">{title}</span>{description && <span className="row-note" id={descriptionId}>{description}</span>}</span>;
}

export function SettingsLink({ title, description, external = false, ...props }: Omit<ComponentProps<typeof Button>, "title" | "children" | "className"> & { title: string; description?: ReactNode; external?: boolean }): React.JSX.Element {
  const Icon = external ? ExternalLink : ChevronRight;
  return <Button type="button" variant="ghost" className="row list-row" {...props}>
    <RowContent title={title} description={description} />
    <Icon size={16} className="row-chevron" aria-hidden="true" />
  </Button>;
}

export function SettingsToggle({ label, description, ...props }: ComponentProps<typeof SwitchControl> & { description?: ReactNode }): React.JSX.Element {
  const descriptionId = useId();
  return <div className="row toggle-row">
    <RowContent title={label} description={description} descriptionId={descriptionId} />
    <SwitchControl size="sm" label={label} aria-describedby={description ? descriptionId : undefined} {...props} />
  </div>;
}

export function FormField({ id, label, description, children }: PropsWithChildren<{ id: string; label: string; description?: ReactNode }>): React.JSX.Element {
  return <div className="row field settings-field-row">
    <label className="field-label" htmlFor={id}>{label}</label>
    <div className="field-controls">{children}</div>
    {description && <span className="field-note" id={`${id}-note`}>{description}</span>}
  </div>;
}
