import { useCallback, useEffect, useRef, useState } from "react";
import type { AccountLogin, ConfidentialProfileInput, DesktopApi, AccountLoginDetails } from "../../shared/contracts";

type LoginApi = Pick<DesktopApi, "beginAccountLogin" | "pollAccountLogin" | "cancelAccountLogin" | "completeAccountLogin">;
type LoginState =
  | { phase: "idle" }
  | { phase: "authorizing"; session: AccountLogin }
  | { phase: "authorized"; session: AccountLogin; details: AccountLoginDetails };

/** Owns a draft authorization, independently of the form's save operation. */
export function useAccountLogin(api: LoginApi, onError: (error: unknown) => void) {
  const [state, setState] = useState<LoginState>({ phase: "idle" });
  const [working, setWorking] = useState(false);
  const session = useRef<AccountLogin | undefined>(undefined);
  const mounted = useRef(false);
  const operation = useRef(false);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const current = session.current;
      session.current = undefined;
      if (current) void api.cancelAccountLogin(current.id).catch(() => console.error("Could not discard account authorization"));
    };
  }, [api]);

  const consume = useCallback(() => {
    session.current = undefined;
    if (mounted.current) setState({ phase: "idle" });
  }, []);

  const discard = useCallback(async () => {
    if (session.current) await api.cancelAccountLogin(session.current.id);
    consume();
  }, [api, consume]);

  const perform = useCallback(async (action: () => Promise<void>) => {
    if (operation.current || !mounted.current) return false;
    operation.current = true;
    setWorking(true);
    try {
      await action();
      return true;
    } catch (error) {
      if (mounted.current) onError(error);
      else console.error("Could not clean up account authorization");
      return false;
    } finally {
      operation.current = false;
      if (mounted.current) setWorking(false);
    }
  }, [onError]);

  const cancel = useCallback(() => perform(discard), [discard, perform]);
  const start = useCallback((profile: ConfidentialProfileInput) => perform(async () => {
    await discard();
    if (!mounted.current) return;
    const created = await api.beginAccountLogin(profile);
    if (!mounted.current) {
      await api.cancelAccountLogin(created.id);
      return;
    }
    session.current = created;
    setState({ phase: "authorizing", session: created });
  }), [api, discard, perform]);

  const complete = useCallback((callbackUrl: string) => perform(async () => {
    const current = session.current;
    if (!current) throw new Error("Account login is no longer active");
    await api.completeAccountLogin(current.id, callbackUrl);
  }), [api, perform]);

  const pending = state.phase === "authorizing" ? state.session : undefined;
  useEffect(() => {
    if (!pending) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const current = () => !disposed && session.current?.id === pending.id;
    const poll = async () => {
      if (!current()) return;
      if (!operation.current) {
        try {
          const details = await api.pollAccountLogin(pending.id);
          if (!current()) return;
          if (details && !operation.current) {
            setState({ phase: "authorized", session: pending, details });
            return;
          }
        } catch (error) {
          if (!current()) return;
          if (!operation.current) {
            onError(error);
            // Keep the handle until cancellation succeeds, so cleanup is retryable.
            await cancel();
            return;
          }
        }
      }
      if (current()) timer = setTimeout(() => void poll(), 1000);
    };
    void poll();
    return () => { disposed = true; clearTimeout(timer); };
  }, [api, cancel, onError, pending]);

  return {
    session: state.phase === "idle" ? undefined : state.session,
    auth: state.phase === "authorized" ? state.details.auth : undefined,
    details: state.phase === "authorized" ? state.details : undefined,
    busy: working || state.phase === "authorizing",
    working,
    start,
    cancel,
    consume,
    complete,
  };
}
