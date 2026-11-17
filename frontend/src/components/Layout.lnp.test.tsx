import { describe, it, expect, vi, beforeEach } from 'vitest';

// The theme is a build-time switch, read when config/theme.ts is first evaluated.
vi.hoisted(() => {
    vi.stubEnv('VITE_THEME', 'lnp');
});

import { render, screen, fireEvent, waitFor, mockFetchResponse } from '../test/test-utils';
import Layout from './Layout';

vi.mock('react-router-dom', async () => {
    const actual = await vi.importActual('react-router-dom');
    return { ...actual, Outlet: () => <div data-testid="outlet">Page Content</div> };
});

describe('Layout in the LNP build: Sign up', () => {
    const serveSettings = (settings: unknown) =>
        vi.mocked(global.fetch).mockImplementation((() => mockFetchResponse(settings)) as any);

    beforeEach(() => {
        vi.mocked(global.fetch).mockReset();
        localStorage.clear();
    });

    it('offers only "Log in" when the instance signs in with Google', async () => {
        serveSettings({ passkeys_enabled: true, google_client_id: 'client-1.apps.googleusercontent.com' });
        render(<Layout />);
        fireEvent.click(screen.getByRole('button', { name: /toggle menu/i }));

        await waitFor(() => {
            expect(global.fetch).toHaveBeenCalledWith(expect.stringMatching(/\/auth\/settings$/));
        });
        await new Promise((resolve) => setTimeout(resolve, 0));
        expect(screen.queryByRole('link', { name: 'Sign up' })).not.toBeInTheDocument();
        const logIn = screen.getAllByRole('link', { name: 'Log in' });
        expect(logIn).toHaveLength(2);
        logIn.forEach(link => expect(link).toHaveAttribute('href', '/login'));
    });

    it('does not show "Sign up" for the moment before the settings arrive', () => {
        vi.mocked(global.fetch).mockImplementation((() => new Promise(() => {})) as any);
        render(<Layout />);

        expect(screen.queryByRole('link', { name: 'Sign up' })).not.toBeInTheDocument();
        expect(screen.getByRole('link', { name: 'Log in' })).toBeInTheDocument();
    });

    it('offers "Sign up" when the settings carry no client id', async () => {
        serveSettings({ passkeys_enabled: true });
        render(<Layout />);
        fireEvent.click(screen.getByRole('button', { name: /toggle menu/i }));

        const signUp = await screen.findAllByRole('link', { name: 'Sign up' });
        expect(signUp).toHaveLength(2);
        signUp.forEach(link => expect(link).toHaveAttribute('href', '/register'));
    });

    it('offers "Sign up" when the settings cannot be read', async () => {
        vi.mocked(global.fetch).mockRejectedValue(new Error('offline'));
        render(<Layout />);

        expect(await screen.findByRole('link', { name: 'Sign up' })).toBeInTheDocument();
    });

    it('does not ask for the settings while signed in', async () => {
        localStorage.setItem('token', 'test-token');
        serveSettings({ google_client_id: 'client-1.apps.googleusercontent.com' });
        render(<Layout />);
        await new Promise((resolve) => setTimeout(resolve, 20));

        expect(global.fetch).not.toHaveBeenCalled();
    });
});
