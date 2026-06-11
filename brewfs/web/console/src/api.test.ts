import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, fetchHealth } from './api';

const healthResponse = {
  service: 'brewfs-console',
  version: '0.1.0',
  commit_short: 'abcdef1',
  auth_mode: 'token',
  integrations: {
    csi_dashboard: false,
  },
  static_assets_available: true,
};

describe('fetchHealth', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('sends a bearer token when one is provided', async () => {
    const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify(healthResponse), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );

    await fetchHealth('secret-token');

    expect(fetch).toHaveBeenCalledWith('/api/health', {
      headers: {
        Accept: 'application/json',
        Authorization: 'Bearer secret-token',
      },
    });
  });

  it('throws an ApiError with status for unauthorized responses', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ error: { code: 'unauthorized' } }), {
        status: 401,
        headers: { 'content-type': 'application/json' },
      }),
    );

    await expect(fetchHealth()).rejects.toMatchObject({
      name: 'ApiError',
      status: 401,
    } satisfies Partial<ApiError>);
  });
});
