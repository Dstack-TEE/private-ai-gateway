import { useId, useLayoutEffect, useState, type FormEvent } from "react";
import { createRoot } from "react-dom/client";
import { brand } from "../brand/brand";
import { errorMessage } from "../lib/error-message";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from "./ui/field";
import { Input } from "./ui/input";

/** Shows the web UI sign-in page until `signIn` accepts a password. */
export function showSignIn(notice: string | undefined, signIn: (password: string) => Promise<void>): Promise<void> {
  return new Promise((resolve) => {
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    root.render(<SignIn notice={notice} signIn={signIn} onSignedIn={() => {
      root.unmount();
      container.remove();
      resolve();
    }} />);
  });
}

function SignIn({ notice, signIn, onSignedIn }: { notice?: string; signIn(password: string): Promise<void>; onSignedIn(): void }) {
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const errorId = useId();
  // Saved appearance needs a session, so the sign-in page follows the system theme.
  useLayoutEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => document.documentElement.classList.toggle("dark", media.matches);
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, []);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      await signIn(password);
      onSignedIn();
    } catch (signInError) {
      setError(errorMessage(signInError));
      setBusy(false);
    }
  };
  return <main className="grid min-h-svh place-items-center bg-background p-4 text-foreground">
    <Card className="w-full max-w-sm">
      <CardHeader>
        {/* Not BrandMark: this renders before the backend, so it must not import the environment. */}
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
            <FieldDescription>Set the password in the desktop app under Settings › Web UI, or with pap settings set web-ui.password.</FieldDescription>
          </FieldGroup>
        </form>
      </CardContent>
    </Card>
  </main>;
}
