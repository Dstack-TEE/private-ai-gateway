import { useId, useState, type FormEvent } from "react";
import { useLocation, useRouter, useSearch } from "@tanstack/react-router";
import { brand } from "../brand/brand";
import { errorMessage } from "../lib/error-message";
import { session } from "../lib/environment";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from "./ui/field";
import { Input } from "./ui/input";

/** The web UI's `/sign-in` page: exchanges the password for a session, then returns to the page asked for. */
export function SignInPage() {
  const router = useRouter();
  const { redirect } = useSearch({ from: "/sign-in" });
  const notice = useLocation({ select: (location) => location.state.notice });
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const errorId = useId();
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      if (!session) throw new Error("Sign-in is only needed in the web UI");
      await session.signIn(password);
      router.history.push(redirect ?? "/");
    } catch (signInError) {
      setError(errorMessage(signInError));
      setBusy(false);
    }
  };
  return <main className="grid min-h-svh place-items-center bg-background p-4 text-foreground">
    <Card className="w-full max-w-sm">
      <CardHeader>
        {/* Saved appearance needs a session, so the mark follows the system theme like the page. */}
        <picture className="mb-2 block size-10" aria-hidden="true">
          <source media="(prefers-color-scheme: dark)" srcSet={brand.appIcon.dark} />
          <img className="size-full object-contain" src={brand.appIcon.light} alt="" />
        </picture>
        <CardTitle className="text-lg">Sign in to {brand.productName}</CardTitle>
        <CardDescription>{notice ?? "Enter the web UI password."}</CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={(event) => void submit(event)}>
          <FieldGroup>
            <Field data-invalid={Boolean(error)}>
              <FieldLabel htmlFor="web-ui-sign-in-password">Password</FieldLabel>
              <Input id="web-ui-sign-in-password" type="password" autoComplete="current-password" autoFocus required value={password} disabled={busy} aria-invalid={Boolean(error)} aria-describedby={error ? errorId : undefined} onChange={(event) => setPassword(event.target.value)} />
              {error && <FieldError id={errorId}>{error}</FieldError>}
            </Field>
            <Button type="submit" disabled={busy || !password}>{busy ? "Signing In…" : "Sign In"}</Button>
            <FieldDescription>Set the password in the desktop app under Settings › Web UI, or with <code>pap settings set web-ui.password</code>.</FieldDescription>
          </FieldGroup>
        </form>
      </CardContent>
    </Card>
  </main>;
}
