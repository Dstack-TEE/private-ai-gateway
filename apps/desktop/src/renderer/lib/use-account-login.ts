import { useEffect, useId, useRef } from "react";
import { QueryObserver, queryOptions, skipToken, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { AccountLoginDetails, ConfidentialProfileInput, DesktopApi, LoginPresentation } from "../../shared/contracts";

type LoginApi = Pick<DesktopApi, "beginAccountLogin" | "pollAccountLogin" | "cancelAccountLogin" | "completeAccountLogin">;

export interface Authorization {
  login: LoginPresentation;
  details: AccountLoginDetails;
}

/**
 * Owns a draft authorization, independently of the form's save operation.
 * Starting, cancelling and completing it run one after another (the mutation
 * scope); failures go to `onError`.
 */
export function useAccountLogin(api: LoginApi, onError: (error: unknown) => void) {
  const client = useQueryClient();
  const scope = { id: `account-login:${useId()}` };
  // The authorization to discard when the form closes, including one that
  // is still being created then, and the wait for it to complete.
  const pending = useRef<LoginPresentation | undefined>(undefined);
  const stopWaiting = useRef<(() => void) | undefined>(undefined);
  const closed = useRef(false);
  useEffect(() => {
    closed.current = false;
    return () => {
      closed.current = true;
      stopWaiting.current?.();
      const login = pending.current;
      pending.current = undefined;
      if (login) void api.cancelAccountLogin(login.id).catch(() => console.error("Could not discard account authorization"));
    };
  }, [api]);

  const discard = async () => {
    stopWaiting.current?.();
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
  // operation on the authorization runs. The user is in the browser, so it
  // keeps polling while the window is in the background.
  const poll = (id: string | undefined) => queryOptions({
    queryKey: ["account-login", id],
    queryFn: id ? () => api.pollAccountLogin(id) : skipToken,
    refetchInterval: (query) => query.state.data || query.state.error ? false : 1_000,
    refetchIntervalInBackground: true,
    retry: false,
    gcTime: 0,
  });
  const { data } = useQuery({ ...poll(session?.id), enabled: !working });
  const details = session ? data ?? undefined : undefined;

  /** Follows the poll above: the account once signed in, or `null` once discarded. */
  const authorization = (login: LoginPresentation) => new Promise<AccountLoginDetails | null>((resolve, reject) => {
    const observer = new QueryObserver(client, { ...poll(login.id), enabled: false });
    const settle = (result: { data?: AccountLoginDetails | null; error: Error | null }) => {
      if (result.data) resolve(result.data);
      else if (result.error) reject(result.error);
      else return;
      stop();
    };
    const unsubscribe = observer.subscribe(settle);
    const stop = () => {
      unsubscribe();
      stopWaiting.current = undefined;
      resolve(null);
    };
    stopWaiting.current = stop;
    settle(observer.getCurrentResult());
  });

  return {
    session,
    auth: details?.auth,
    details,
    busy: working || Boolean(session && !details),
    working,
    /**
     * Signs in for `profile` and resolves once the browser authorization
     * completes, or with `null` when it is cancelled. A failed sign-in is
     * discarded before the promise rejects.
     */
    authorize: async (profile: ConfidentialProfileInput): Promise<Authorization | null> => {
      const login = await start.mutateAsync(profile);
      try {
        const details = await authorization(login);
        return details && { login, details };
      } catch (error) {
        await cancel.mutateAsync();
        throw error;
      }
    },
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
