import { describe, it, expect, vi, beforeEach } from 'vitest';

// The theme is a build-time switch, read when config/theme.ts is first evaluated.
vi.hoisted(() => {
  vi.stubEnv('VITE_THEME', 'lnp');
});

import { render, screen, waitFor, mockFetchResponse } from '../test/test-utils';
import Register from './Register';

describe('Register page in the LNP build', () => {
  beforeEach(() => {
    vi.mocked(global.fetch).mockReset();
    vi.mocked(localStorage.getItem).mockReturnValue(null);
  });

  it('goes to /login and shows no password form when the instance signs in with Google', async () => {
    vi.mocked(global.fetch).mockImplementation((() =>
      mockFetchResponse({ google_client_id: 'client-1.apps.googleusercontent.com' })) as any);
    render(<Register />, { route: '/register' });

    await waitFor(() => {
      expect(window.location.pathname).toBe('/login');
    });
    expect(global.fetch).toHaveBeenCalledWith(expect.stringMatching(/\/auth\/settings$/));
    expect(screen.queryByRole('heading', { name: /create your account/i })).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/password/i)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /create account/i })).not.toBeInTheDocument();
  });

  it('shows no form for the moment before the settings arrive', () => {
    vi.mocked(global.fetch).mockImplementation((() => new Promise(() => {})) as any);
    render(<Register />, { route: '/register' });

    expect(screen.queryByRole('button', { name: /create account/i })).not.toBeInTheDocument();
  });

  it('stays on the sign-up form when the settings carry no client id', async () => {
    vi.mocked(global.fetch).mockImplementation((() => mockFetchResponse({ passkeys_enabled: true })) as any);
    render(<Register />, { route: '/register' });

    expect(await screen.findByRole('button', { name: /create account/i })).toBeInTheDocument();
    expect(window.location.pathname).toBe('/register');
  });
});
