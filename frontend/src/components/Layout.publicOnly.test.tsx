import { beforeEach, describe, expect, it, vi } from 'vitest';

// Build-time switches, read when config/theme.ts is first evaluated: the LNP
// image for the host that only serves short links.
vi.hoisted(() => {
    vi.stubEnv('VITE_THEME', 'lnp');
    vi.stubEnv('VITE_PUBLIC_LINKS_ONLY', 'true');
});

import { render, screen } from '../test/test-utils';
import Layout from './Layout';

vi.mock('react-router-dom', async () => {
    const actual = await vi.importActual('react-router-dom');
    return { ...actual, Outlet: () => <div data-testid="outlet">Page Content</div> };
});

describe('Layout on a host that only serves short links', () => {
    beforeEach(() => {
        vi.mocked(global.fetch).mockReset();
        localStorage.clear();
    });

    it('shows the logo and the page, and nothing that leads to an account', async () => {
        render(<Layout />);

        expect(screen.getByAltText('Leadership Now Project')).toBeInTheDocument();
        expect(screen.getByTestId('outlet')).toBeInTheDocument();
        for (const name of ['Log in', 'Sign up', 'Dashboard', 'Settings', 'Features', 'Pricing']) {
            expect(screen.queryByRole('link', { name })).not.toBeInTheDocument();
        }
        expect(screen.queryByRole('button', { name: /toggle menu/i })).not.toBeInTheDocument();
        expect(screen.queryByRole('button', { name: /account menu/i })).not.toBeInTheDocument();
        for (const link of screen.getAllByRole('link')) {
            expect(link.getAttribute('href')).not.toMatch(/^\/(login|register|dashboard|settings)/);
        }
    });

    it('asks the server for nothing: the sign-in settings are not on this host', async () => {
        render(<Layout />);
        await new Promise((resolve) => setTimeout(resolve, 0));

        expect(global.fetch).not.toHaveBeenCalled();
    });

    it('has no account menu even in a browser that holds a session from elsewhere', () => {
        localStorage.setItem('token', 'a-token');
        localStorage.setItem('is_admin', 'true');
        render(<Layout />);

        expect(screen.queryByRole('button', { name: /account menu/i })).not.toBeInTheDocument();
        expect(screen.queryByRole('link', { name: /admin/i })).not.toBeInTheDocument();
    });
});
