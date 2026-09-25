import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { LoginPresentation, ConfidentialProfileInput, DesktopApi } from "../../shared/contracts";

type LoginApi = Pick<DesktopApi, "beginAccountLogin" | "pollAccountLogin" | "cancelAccountLogin" | "completeAccountLogin">;

/** Owns a draft authorization, independently of the form's save operation. */
export function useAccountLogin(api: LoginApi, onError: (error: unknown) => void) {
  const [session, setSession] = useState<LoginPresentation>();
  const [working, setWorking] = useState(false);
  const current = useRef<LoginPresentation | undefined>(undefined);
  const mounted = useRef(false);
  const operation = useRef(false);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const pending = current.current;
      current.current = undefined;
      if (pending) void api.cancelAccountLogin(pending.id).catch(() => console.error("Could not discard account authorization"));
    };
  }, [api]);

  const consume = useCallback(() => {
    current.current = undefined;
    if (mounted.current) setSession(undefined);
  }, []);

  const discard = useCallback(async () => {
    if (current.current) await api.cancelAccountLogin(current.current.id);
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
    current.current = created;
    setSession(created);
  }), [api, discard, perform]);

  const complete = useCallback((callbackUrl: string) => perform(async () => {
    if (!current.current) throw new Error("Account connection is no longer active");
    await api.completeAccountLogin(current.current.id, callbackUrl);
  }), [api, perform]);

  // Polls until the browser sign-in completes; pauses while another
  // operation on the authorization runs.
  const poll = useQuery({
    queryKey: ["account-login", session?.id],
    queryFn: () => api.pollAccountLogin(session?.id ?? ""),
    enabled: Boolean(session) && !working,
    refetchInterval: (query) => query.state.data ? false : 1_000,
    retry: false,
    gcTime: 0,
  });
  const details = session ? poll.data ?? undefined : undefined;
  const pollError = poll.error;
  useEffect(() => {
    if (!pollError) return;
    onError(pollError);
    // Keep the handle until cancellation succeeds, so cleanup is retryable.
    void cancel();
  }, [pollError, onError, cancel]);

  return {
    session,
    auth: details?.auth,
    details,
    busy: working || Boolean(session && !details),
    working,
    start,
    cancel,
    consume,
    complete,
  };
}
