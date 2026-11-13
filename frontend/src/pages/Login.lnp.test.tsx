import { describe, it, expect, vi, beforeEach } from 'vitest';

// The theme is a build-time switch (VITE_THEME), read when config/theme.ts is
// first evaluated. Set it before anything imports that module.
vi.hoisted(() => {
  vi.stubEnv('VITE_THEME', 'lnp');
});

import { render, screen, waitFor } from '../test/test-utils';
import Login from './Login';
import { IS_LNP } from '../config/theme';

describe('Login page under the LNP theme', () => {
  beforeEach(() => {
    vi.mocked(global.fetch).mockReset();
    vi.mocked(localStorage.getItem).mockReturnValue(null);
  });

  it('names the LNP dashboard, not opn.onl, in its heading', async () => {
    expect(IS_LNP).toBe(true);
    render(<Login />);
    await waitFor(() => {
      expect(screen.queryByRole('status', { name: /loading sign-in options/i })).not.toBeInTheDocument();
    });

    const heading = screen.getByRole('heading', { level: 1 });
    expect(heading).toHaveTextContent('Sign in to LNP Links');
    expect(heading).not.toHaveTextContent('opn.onl');
  });
});
