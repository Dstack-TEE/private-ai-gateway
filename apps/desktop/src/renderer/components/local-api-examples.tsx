import { Fragment, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, Copy } from "lucide-react";
import { exampleModels, localApiExample, type ExampleKind, type ExampleLanguage } from "../lib/local-api-example";
import { useCopy } from "../hooks/use-copy";
import { AppDialog, DoneFooter, type DialogControl } from "./app-dialog";
import { IconButton } from "./controls";
import { Field, FieldError, FieldLabel } from "./ui/field";
import { ChoiceSelect } from "./choice-select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "./ui/tabs";
import { errorMessage } from "../lib/error-message";
import { localEndpoint } from "../lib/format";
import { useShell } from "../lib/shell";

export function LocalApiExamplesDialog(control: DialogControl) {
  const { state, clientKey: apiKey } = useShell();
  const endpoint = state.proxyUrl ?? localEndpoint(state.localApi);
  const catalog = state.catalog?.models ?? [];
  const decisionsAvailable = exampleModels("decisions", catalog).length > 0;
  const [tab, setTab] = useState<ExampleKind>("chat");
  const kind = decisionsAvailable ? tab : "chat";
  const models = exampleModels(kind, catalog);
  const [language, setLanguage] = useState<ExampleLanguage>("javascript");
  const [selection, setSelection] = useState("");
  const model = models.some((entry) => entry.id === selection) ? selection : models[0]?.id ?? "";
  const code = useMemo(() => {
    if (!endpoint || !apiKey) return undefined;
    try { return localApiExample(kind, language, endpoint, model || "MODEL_ID", apiKey); }
    catch { return undefined; }
  }, [endpoint, kind, language, model, apiKey]);
  // Loaded with the dialog; the code shows plain until it is ready, or if it fails.
  const { data: highlight } = useQuery({
    queryKey: ["example-highlighter"],
    queryFn: () => import("../lib/example-highlighter").then((module) => module.createExampleHighlighter()),
    staleTime: Infinity,
    gcTime: Infinity,
    retry: false,
  });
  const tokens = useMemo(() => {
    if (!code || !highlight) return undefined;
    try { return highlight(code, language); }
    catch { return undefined; }
  }, [code, highlight, language]);
  const { copy, copying, error, isCopied, status } = useCopy();
  return <AppDialog {...control} title="Local API examples" className="sm:max-w-3xl">
    <Tabs value={kind} className="min-h-0 flex-1 gap-4" onValueChange={(value) => { if (value === "chat" || value === "decisions") setTab(value); }}>
      <TabsList aria-label="Example" className="w-full"><TabsTrigger value="chat">Chat</TabsTrigger><TabsTrigger value="decisions" disabled={!decisionsAvailable}>Decisions</TabsTrigger></TabsList>
      <TabsContent value={kind} className="flex min-h-0 flex-col gap-4">
        <Field>
          <FieldLabel htmlFor="example-model">Model</FieldLabel>
          <ChoiceSelect id="example-model" label="Model" className="w-full" value={model} disabled={models.length === 0} onChange={setSelection}
            options={models.length ? models.map((entry) => ({ value: entry.id, label: entry.name || entry.id })) : [{ value: "", label: "No verified models" }]} />
        </Field>
        <Tabs value={language} className="min-h-0 flex-1" onValueChange={(value) => { if (value === "curl" || value === "python" || value === "javascript") setLanguage(value); }}>
          <div className="flex items-center justify-between gap-2">
            <TabsList aria-label="Code language"><TabsTrigger value="javascript">JavaScript</TabsTrigger><TabsTrigger value="python">Python</TabsTrigger><TabsTrigger value="curl">cURL</TabsTrigger></TabsList>
            <IconButton label="Copy example" disabled={!code || copying} onClick={() => { if (code) copy("Example", code); }}>{isCopied(code) ? <Check /> : <Copy />}</IconButton>
          </div>
          <TabsContent value={language} className="min-h-0 overflow-auto rounded-2xl border bg-muted/50">
            <pre className="p-4 text-xs leading-relaxed"><code>{tokens
              ? tokens.map((line, index) => <Fragment key={index}>{index > 0 && "\n"}{line.map((token) => <span key={token.offset} className="text-(--shiki-light) dark:text-(--shiki-dark)" style={token.htmlStyle}>{token.content}</span>)}</Fragment>)
              : code ?? "Local API example unavailable."}</code></pre>
          </TabsContent>
        </Tabs>
      </TabsContent>
    </Tabs>
    <FieldError>{apiKey ? (error ? `Could not copy the example. ${errorMessage(error)}` : undefined) : "The Local API key is unavailable."}</FieldError>
    {status}
    <DoneFooter />
  </AppDialog>;
}
