export type AuthMode = 'disabled' | 'token';

export interface HealthResponse {
  service: 'brewfs-console';
  version: string;
  commit_short: string;
  auth_mode: AuthMode;
  integrations: {
    csi_dashboard: boolean;
  };
  static_assets_available: boolean;
}

export class ApiError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

export async function fetchHealth(token?: string | null): Promise<HealthResponse> {
  const headers: Record<string, string> = { Accept: 'application/json' };
  const trimmedToken = token?.trim();
  if (trimmedToken) {
    headers.Authorization = `Bearer ${trimmedToken}`;
  }

  const response = await fetch('/api/health', {
    headers,
  });

  if (!response.ok) {
    throw new ApiError(`health request failed: ${response.status}`, response.status);
  }

  return (await response.json()) as HealthResponse;
}
