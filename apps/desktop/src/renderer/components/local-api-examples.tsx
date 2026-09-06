import { useEffect, useMemo, useState } from "react";
import { Check, Copy } from "lucide-react";
import type { ModelSummary, DesktopApi } from "../../shared/contracts";
import { localApiExample, type ExampleLanguage } from "../lib/local-api-example";
import { Sheet, DismissSheetAction } from "./sheet";
import { StateLabel } from "./state-label";
import { IconButton } from "./controls";
import { Field, FieldError, FieldLabel } from "./ui/field";
import { NativeSelect } from "./ui/native-select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "./ui/tabs";

export function LocalApiExamples({ endpoint, available, models, api, onCopy, onClose }: {
  api: Pick<DesktopApi, "getClientKey" | "onClientKeyChange">;
  endpoint?: string; available: boolean; models: ModelSummary[];
  onCopy(value: string): Promise<void>; onClose(): void;
}) {
  const [language, setLanguage] = useState<ExampleLanguage>("curl");
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
      <div className="flex items-center justify-between gap-3"><FieldLabel htmlFor="example-model">Model</FieldLabel><StateLabel tone={available ? "success" : "neutral"} text={available ? "Available" : "Unavailable"} /></div>
      <NativeSelect id="example-model" className="w-full" value={model} disabled={models.length === 0} onChange={(event) => setSelection(event.target.value)}>
        {models.length === 0 && <option value="">No verified models</option>}
        {models.map((entry) => <option key={entry.id} value={entry.id}>{entry.name || entry.id}</option>)}
      </NativeSelect>
    </Field>
    <Tabs value={language} className="mt-4 min-h-0 flex-1" onValueChange={(value) => { if (value === "curl" || value === "python" || value === "javascript") setLanguage(value); }}>
      <div className="flex items-center justify-between gap-2">
        <TabsList aria-label="Code language"><TabsTrigger value="curl">cURL</TabsTrigger><TabsTrigger value="python">Python</TabsTrigger><TabsTrigger value="javascript">JavaScript</TabsTrigger></TabsList>
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
