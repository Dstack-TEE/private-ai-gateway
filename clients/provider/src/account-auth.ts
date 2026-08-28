export interface AccountApiKeyCredential {
  apiKey: string;
  metadata?: Record<string, string>;
}

export interface StartAccountApiKeyAuthorizationOptions {
  signal?: AbortSignal;
}

export interface CompleteAccountApiKeyAuthorizationOptions {
  signal?: AbortSignal;
  onProgress?: (message: string) => void;
}

export type AccountApiKeyAuthorizationPresentation =
  | { type: "authorization_url" }
  | {
      type: "device_code";
      userCode: string;
      intervalSeconds?: number;
      expiresInSeconds?: number;
    };

export interface AccountApiKeyAuthorization {
  url: string;
  instructions?: string;
  presentation: AccountApiKeyAuthorizationPresentation;
  complete(options?: CompleteAccountApiKeyAuthorizationOptions): Promise<AccountApiKeyCredential>;
}

/** Product account flow that issues one inference API key to the host. */
export interface AccountApiKeyAuth {
  label: string;
  start(options?: StartAccountApiKeyAuthorizationOptions): Promise<AccountApiKeyAuthorization>;
}
