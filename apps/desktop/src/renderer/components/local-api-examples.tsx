import { useEffect, useMemo, useState } from "react";
import { Check, Copy } from "lucide-react";
import type { ModelSummary, DesktopApi } from "../../shared/contracts";
import { localApiExample, type ExampleLanguage } from "../lib/local-api-example";
import { Sheet, DismissSheetAction } from "./sheet";
import { IconButton } from "./controls";
import { Field, FieldError, FieldLabel } from "./ui/field";
import { ChoiceSelect } from "./choice-select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "./ui/tabs";

export function LocalApiExamples({ endpoint, models, api, onCopy, onClose }: {
  api: Pick<DesktopApi, "getClientKey" | "onClientKeyChange">;
  endpoint?: string; models: ModelSummary[];
  onCopy(value: string): Promise<void>; onClose(): void;
}) {
  const [language, setLanguage] = useState<ExampleLanguage>("javascript");
  const [selection, setSelection] = useState("");
  const [copied, setCopied] = useState<string>();
  const [copying, setCopying] = useState(false);
  const [error, setError] = useState<string>();
  const [apiKey, setApiKey] = useState<string>();
  useEffect(() => {
    let active = true;
    let generation = 0;
    const load = () => {
      const current = ++generation;
      setApiKey(undefined);
      setCopied(undefined);
      void api.getClientKey().then((key) => { if (active && current === generation) setApiKey(key); }).catch(() => { if (active) setError("Local API key is unavailable."); });
    };
    const unsubscribe = api.onClientKeyChange(load);
    load();
    return () => { active = false; unsubscribe(); };
  }, [api]);
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
    setError(undefined);
    try { await onCopy(code); setCopied(code); }
    catch { setError("Could not copy the example."); }
    finally { setCopying(false); }
  };
  return <Sheet title="Local API examples" className="local-api-examples-sheet" onClose={onClose}>
    <Field className="mt-4">
      <FieldLabel htmlFor="example-model">Model</FieldLabel>
      <ChoiceSelect id="example-model" label="Model" className="w-full" value={model} disabled={models.length === 0} onChange={setSelection}
        options={models.length ? models.map((entry) => ({ value: entry.id, label: entry.name || entry.id })) : [{ value: "", label: "No verified models" }]} />
    </Field>
    <Tabs value={language} className="mt-4 min-h-0 flex-1" onValueChange={(value) => { if (value === "curl" || value === "python" || value === "javascript") setLanguage(value); }}>
      <div className="flex items-center justify-between gap-2">
        <TabsList aria-label="Code language"><TabsTrigger value="javascript">JavaScript</TabsTrigger><TabsTrigger value="python">Python</TabsTrigger><TabsTrigger value="curl">cURL</TabsTrigger></TabsList>
        <IconButton label="Copy example" disabled={!code || copying} onClick={() => void copy()}>{copied === code && code ? <Check className="text-success" /> : <Copy />}</IconButton>
      </div>
      <TabsContent value={language} className="min-h-0 overflow-auto rounded-2xl border bg-muted/50">
        <pre className="p-4 text-xs leading-relaxed"><code>{code ?? "Local API example unavailable."}</code></pre>
      </TabsContent>
    </Tabs>
    <FieldError>{error}</FieldError>
    <span className="sr-only" role="status">{copied === code && code ? "Example copied" : ""}</span>
    <DismissSheetAction onClose={onClose} />
  </Sheet>;
}
