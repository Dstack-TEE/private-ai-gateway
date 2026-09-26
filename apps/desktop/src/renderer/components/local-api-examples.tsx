import { useEffect, useMemo, useState } from "react";
import { Check, Copy } from "lucide-react";
import type { ModelSummary } from "../../shared/contracts";
import { localApiExample, type ExampleLanguage } from "../lib/local-api-example";
import { AppDialog, DoneFooter, type DialogControl } from "./app-dialog";
import { IconButton } from "./controls";
import { Field, FieldError, FieldLabel } from "./ui/field";
import { ChoiceSelect } from "./choice-select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "./ui/tabs";

export function LocalApiExamplesDialog({ endpoint, models: catalogModels, apiKey, onCopy, ...control }: {
  /** The Local API key; empty while it is unavailable. */
  apiKey: string;
  endpoint?: string; models: ModelSummary[];
  onCopy(value: string): Promise<void>;
} & DialogControl) {
  const models = catalogModels.filter((model) => model.supportedEndpoints?.includes("/v1/chat/completions") ?? true);
  const [language, setLanguage] = useState<ExampleLanguage>("javascript");
  const [selection, setSelection] = useState("");
  const [copied, setCopied] = useState<string>();
  const [copying, setCopying] = useState(false);
  const [error, setError] = useState<string>();
  const model = models.some((entry) => entry.id === selection) ? selection : models[0]?.id ?? "";
  const code = useMemo(() => {
    if (!endpoint || !apiKey) return undefined;
    try { return localApiExample(language, endpoint, model || "MODEL_ID", apiKey); }
    catch { return undefined; }
  }, [endpoint, language, model, apiKey]);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(undefined), 1_500);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const copy = async () => {
    if (!code || copying) return;
    setCopying(true);
    try { await onCopy(code); setCopied(code); }
    catch { setError("Could not copy the example."); }
    finally { setCopying(false); }
  };
  return <AppDialog {...control} title="Local API examples" className="sm:max-w-3xl">
    <Field>
      <FieldLabel htmlFor="example-model">Model</FieldLabel>
      <ChoiceSelect id="example-model" label="Model" className="w-full" value={model} disabled={models.length === 0} onChange={setSelection}
        options={models.length ? models.map((entry) => ({ value: entry.id, label: entry.name || entry.id })) : [{ value: "", label: "No verified models" }]} />
    </Field>
    <Tabs value={language} className="min-h-0 flex-1" onValueChange={(value) => { if (value === "curl" || value === "python" || value === "javascript") setLanguage(value); }}>
      <div className="flex items-center justify-between gap-2">
        <TabsList aria-label="Code language"><TabsTrigger value="javascript">JavaScript</TabsTrigger><TabsTrigger value="python">Python</TabsTrigger><TabsTrigger value="curl">cURL</TabsTrigger></TabsList>
        <IconButton label="Copy example" disabled={!code || copying} onClick={() => void copy()}>{copied === code && code ? <Check /> : <Copy />}</IconButton>
      </div>
      <TabsContent value={language} className="min-h-0 overflow-auto rounded-2xl border bg-muted/50">
        <pre className="p-4 text-xs leading-relaxed"><code>{code ?? "Local API example unavailable."}</code></pre>
      </TabsContent>
    </Tabs>
    <FieldError>{apiKey ? error : "The Local API key is unavailable."}</FieldError>
    <span className="sr-only" role="status">{copied === code && code ? "Example copied" : ""}</span>
    <DoneFooter />
  </AppDialog>;
}
