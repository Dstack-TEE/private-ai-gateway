import { useEffect, useId, useRef } from "react";
import { skipToken, useMutation, useQuery } from "@tanstack/react-query";
import type { ConfidentialProfileInput, DesktopApi, LoginPresentation } from "../../shared/contracts";

type LoginApi = Pick<DesktopApi, "beginAccountLogin" | "pollAccountLogin" | "cancelAccountLogin" | "completeAccountLogin">;

/**
 * Owns a draft authorization, independently of the form's save operation.
 * Its operations run one after another (the mutation scope); failures go to
 * `onError`.
 */
export function useAccountLogin(api: LoginApi, onError: (error: unknown) => void) {
  const scope = { id: `account-login:${useId()}` };
  // The authorization to discard when the form closes, including one that
  // is still being created then.
  const pending = useRef<LoginPresentation | undefined>(undefined);
  const closed = useRef(false);
  useEffect(() => {
    closed.current = false;
    return () => {
      closed.current = true;
      const login = pending.current;
      pending.current = undefined;
      if (login) void api.cancelAccountLogin(login.id).catch(() => console.error("Could not discard account authorization"));
    };
  }, [api]);

  const discard = async () => {
    const login = pending.current;
    if (login) await api.cancelAccountLogin(login.id);
    pending.current = undefined;
  };
  const start = useMutation({
    scope,
    mutationFn: async (profile: ConfidentialProfileInput) => {
      await discard();
      const login = await api.beginAccountLogin(profile);
      if (closed.current) await api.cancelAccountLogin(login.id);
      else pending.current = login;
      return login;
    },
    onError,
  });
  const cancel = useMutation({ scope, mutationFn: discard, onSuccess: () => start.reset(), onError });
  const complete = useMutation({
    scope,
    mutationFn: (callbackUrl: string) => {
      if (!pending.current) throw new Error("Account connection is no longer active");
      return api.completeAccountLogin(pending.current.id, callbackUrl);
    },
    onError,
  });
  const working = start.isPending || cancel.isPending || complete.isPending;
  const session = start.data;

  // Polls until the browser sign-in completes or fails; pauses while another
  // operation on the authorization runs.
  const poll = useQuery({
    queryKey: ["account-login", session?.id],
    queryFn: session && !working ? () => api.pollAccountLogin(session.id) : skipToken,
    refetchInterval: (query) => query.state.data || query.state.error ? false : 1_000,
    retry: false,
    gcTime: 0,
  });
  const details = session ? poll.data ?? undefined : undefined;

  return {
    session,
    auth: details?.auth,
    details,
    /** Why the sign-in cannot complete; cancel it and connect again. */
    error: session ? poll.error : null,
    busy: working || Boolean(session && !details && !poll.error),
    working,
    /** Resolves whether a new authorization started. */
    start: (profile: ConfidentialProfileInput) => start.mutateAsync(profile).then(() => true, () => false),
    /** Resolves whether no authorization is left. */
    cancel: () => cancel.mutateAsync().then(() => true, () => false),
    /** The saved profile took the authorization. */
    consume: () => {
      pending.current = undefined;
      start.reset();
    },
    complete: (callbackUrl: string) => complete.mutate(callbackUrl),
  };
}
