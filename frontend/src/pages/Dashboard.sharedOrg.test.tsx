import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '../test/test-utils';
import { mockToken } from '../test/test-utils';
import Dashboard from './Dashboard';

// The instance's shared organization (GET /auth/settings `default_org`): its
// members list, file and create there, and switch the list between their own
// links and everyone's.

const SETTINGS = {
    custom_aliases_enabled: false,
    min_alias_length: 4,
    max_alias_length: 50,
    qr_branding_enabled: false,
    burn_after_reading_enabled: false,
    safe_link_interstitial_enabled: false,
    conditional_routing_enabled: false,
    link_in_bio_enabled: false,
};
const SHARED = { id: 7, name: 'Leadership Now Project' };

interface Row {
    id: number;
    code: string;
    title: string;
    created_by?: { name: string | null; email: string };
    can_delete?: boolean;
}

function link(row: Row) {
    return {
        original_url: `https://example.com/${row.id}`,
        short_url: `http://localhost:3000/${row.code}`,
        click_count: 1,
        created_at: '2026-01-01T00:00:00Z',
        expires_at: null,
        has_password: false,
        notes: null,
        is_active: true,
        is_pinned: false,
        tags: [],
        folder_id: null,
        org_id: SHARED.id,
        ...row,
    };
}

// What the server holds: the viewer's own link and two colleagues'.
const MINE = link({ id: 1, code: 'mine1', title: 'My own link', created_by: { name: null, email: 'me@example.org' }, can_delete: true });
const NAMED = link({ id: 2, code: 'ada22', title: 'Ada made this', created_by: { name: 'Ada Lovelace', email: 'ada@example.org' }, can_delete: false });
const UNNAMED = link({ id: 3, code: 'bob33', title: 'Bob made this', created_by: { name: null, email: 'bob@example.org' }, can_delete: false });

function json(body: unknown, status = 200, headers: Record<string, string> = {}) {
    return new Response(JSON.stringify(body), {
        status,
        headers: { 'Content-Type': 'application/json', ...headers },
    });
}

interface Served {
    /** `default_org` in the settings, or none. */
    defaultOrg?: typeof SHARED | null;
    /** The caller's teams (GET /orgs). */
    orgs?: { id: number; name: string; slug: string; role?: string }[];
    /** Everyone's links in the shared organization. */
    all?: ReturnType<typeof link>[];
}

function serve({ defaultOrg = SHARED, orgs, all = [MINE, NAMED, UNNAMED] }: Served = {}) {
    const teams = orgs ?? [{ ...SHARED, slug: 'lnp', role: 'editor' }];
    vi.mocked(fetch).mockImplementation(async (input, init) => {
        const url = new URL(String(input));
        const method = init?.method ?? 'GET';
        if (url.pathname === '/auth/settings') {
            return json(defaultOrg ? { ...SETTINGS, default_org: defaultOrg } : SETTINGS);
        }
        if (url.pathname === '/auth/me') return json({ total_clicks: 0 });
        if (url.pathname === '/links/sparklines') return json({ sparklines: [] });
        if (url.pathname === '/orgs') return json(teams);
        if (url.pathname === '/folders' || url.pathname === '/tags') return json([]);
        if (url.pathname === '/links' && method === 'GET') {
            // As the server does: an organization's list is everyone's, or
            // the caller's with `mine=true`; no org is the caller's own.
            const inOrg = url.searchParams.get('org_id') === String(SHARED.id);
            const rows = inOrg && url.searchParams.get('mine') !== 'true' ? all : all.filter(l => l.id === MINE.id);
            return json(rows, 200, { 'X-Total-Count': String(rows.length) });
        }
        if (url.pathname === '/links' && method === 'POST') return json(link({ id: 99, code: 'new99', title: 'New' }), 201);
        return json({});
    });
}

/** The query strings of the GET /links requests made so far. */
function listRequests(): URLSearchParams[] {
    return vi.mocked(fetch).mock.calls
        .map(([input, init]) => ({ url: new URL(String(input)), method: init?.method ?? 'GET' }))
        .filter(({ url, method }) => url.pathname === '/links' && method === 'GET')
        .map(({ url }) => url.searchParams);
}

const lastList = () => listRequests().at(-1)!;
const toggle = () => screen.queryByRole('radiogroup', { name: 'Links to show' });
const option = (name: string) => within(toggle()!).getByRole('radio', { name });

async function ready() {
    const view = render(<Dashboard />, { route: '/dashboard' });
    await waitFor(() => expect(screen.getByRole('heading', { name: 'Shared Links' })).toBeInTheDocument());
    return view;
}

/** Wait for the shared organization's list to be the one on screen. */
async function sharedListLoaded() {
    await waitFor(() => expect(toggle()).toBeInTheDocument());
    await waitFor(() => expect(lastList().get('org_id')).toBe(String(SHARED.id)));
}

describe('Dashboard with a shared organization', () => {
    beforeEach(() => {
        vi.mocked(fetch).mockReset();
        localStorage.clear();
        vi.mocked(localStorage.getItem).mockImplementation((key: string) =>
            key === 'token' ? mockToken : stored[key] ?? null);
        vi.mocked(localStorage.setItem).mockImplementation((key: string, value: string) => {
            stored[key] = value;
        });
        for (const key of Object.keys(stored)) delete stored[key];
    });
    // The page's own writes to storage, so a remount reads them back.
    const stored: Record<string, string> = {};

    it('shows no toggle when the instance has no shared organization', async () => {
        serve({ defaultOrg: null, orgs: [{ id: 2, name: 'Acme', slug: 'acme', role: 'editor' }] });
        await ready();
        await waitFor(() => expect(screen.getByText('My own link')).toBeInTheDocument());
        await waitFor(() => expect(screen.getByRole('combobox', { name: 'Link context' })).toBeInTheDocument());

        expect(toggle()).not.toBeInTheDocument();
        // The list is the caller's own, asked for as it always was.
        for (const request of listRequests()) {
            expect(request.has('org_id')).toBe(false);
            expect(request.has('mine')).toBe(false);
        }
        // The team picker still offers the personal list.
        expect(within(screen.getByRole('combobox', { name: 'Link context' })).getByRole('option', { name: 'Personal' })).toBeInTheDocument();
    });

    it('shows no toggle to an account that is not a member of the shared organization', async () => {
        serve({ orgs: [] });
        await ready();
        await waitFor(() => expect(screen.getByText('My own link')).toBeInTheDocument());
        await waitFor(() => expect(fetch).toHaveBeenCalledWith(expect.stringMatching(/\/orgs$/), expect.anything()));
        await new Promise((resolve) => setTimeout(resolve, 20));

        expect(toggle()).not.toBeInTheDocument();
        for (const request of listRequests()) expect(request.has('org_id')).toBe(false);
    });

    it('opens on My Links and asks the server for the caller\'s links in the organization', async () => {
        serve();
        await ready();
        await sharedListLoaded();

        expect(option('My Links')).toHaveAttribute('aria-checked', 'true');
        expect(option('All team links')).toHaveAttribute('aria-checked', 'false');
        expect(lastList().get('mine')).toBe('true');
        await waitFor(() => expect(screen.getByText('My own link')).toBeInTheDocument());
        expect(screen.queryByText('Ada made this')).not.toBeInTheDocument();
        // Your own list does not need to say who made each link.
        expect(screen.queryByText(/Created by/)).not.toBeInTheDocument();
        // One team and no personal list: nothing to pick.
        expect(screen.queryByRole('combobox', { name: 'Link context' })).not.toBeInTheDocument();
    });

    it('switches to everyone\'s links on the server and remembers the choice', async () => {
        serve();
        const { user, unmount } = await ready();
        await sharedListLoaded();
        const before = listRequests().length;

        await user.click(option('All team links'));

        await waitFor(() => expect(listRequests().length).toBeGreaterThan(before));
        expect(lastList().get('org_id')).toBe(String(SHARED.id));
        expect(lastList().has('mine')).toBe(false);
        expect(option('All team links')).toHaveAttribute('aria-checked', 'true');
        expect(option('My Links')).toHaveAttribute('aria-checked', 'false');
        await waitFor(() => expect(screen.getByText('Ada made this')).toBeInTheDocument());
        expect(stored['dashboard.linkView']).toBe('all');

        // A later visit in this browser opens on the remembered view.
        unmount();
        vi.mocked(fetch).mockClear();
        await ready();
        await sharedListLoaded();
        expect(option('All team links')).toHaveAttribute('aria-checked', 'true');
        expect(lastList().has('mine')).toBe(false);
        // No request of this visit asked for "mine" inside the organization.
        expect(listRequests().some(request => request.get('mine') === 'true')).toBe(false);

        // And back again.
        await user.click(option('My Links'));
        await waitFor(() => expect(lastList().get('mine')).toBe('true'));
        expect(stored['dashboard.linkView']).toBe('mine');
        await waitFor(() => expect(screen.queryByText('Ada made this')).not.toBeInTheDocument());
    });

    it('moves between the two views with the arrow keys', async () => {
        serve();
        const { user } = await ready();
        await sharedListLoaded();

        option('My Links').focus();
        await user.keyboard('{ArrowRight}');

        expect(option('All team links')).toHaveAttribute('aria-checked', 'true');
        expect(option('All team links')).toHaveFocus();
        await waitFor(() => expect(lastList().has('mine')).toBe(false));
    });

    it('names who created each link in everyone\'s list, by name or else by email', async () => {
        stored['dashboard.linkView'] = 'all';
        serve();
        await ready();
        await sharedListLoaded();
        await waitFor(() => expect(screen.getByText('Ada made this')).toBeInTheDocument());

        const creators = screen.getAllByText(/^Created by/).map(element => element.textContent);
        expect(creators).toEqual(expect.arrayContaining([
            'Created by Ada Lovelace',
            'Created by bob@example.org',
            'Created by me@example.org',
        ]));
        expect(creators).toHaveLength(3);
    });

    it('offers Edit on a colleague\'s link but Delete only where the server allows it', async () => {
        stored['dashboard.linkView'] = 'all';
        serve();
        await ready();
        await sharedListLoaded();
        await waitFor(() => expect(screen.getByText('Ada made this')).toBeInTheDocument());

        // A colleague's link: change it, do not delete it.
        expect(screen.getByRole('button', { name: 'Edit link ada22' })).toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Pin link ada22' })).toBeInTheDocument();
        expect(screen.queryByRole('button', { name: 'Delete link ada22' })).not.toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Edit link bob33' })).toBeInTheDocument();
        expect(screen.queryByRole('button', { name: 'Delete link bob33' })).not.toBeInTheDocument();
        // The viewer's own: both.
        expect(screen.getByRole('button', { name: 'Edit link mine1' })).toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Delete link mine1' })).toBeInTheDocument();
    });

    it('offers Delete on every link to an organization admin', async () => {
        stored['dashboard.linkView'] = 'all';
        serve({
            orgs: [{ ...SHARED, slug: 'lnp', role: 'admin' }],
            all: [MINE, { ...NAMED, can_delete: true }, { ...UNNAMED, can_delete: true }],
        });
        await ready();
        await sharedListLoaded();
        await waitFor(() => expect(screen.getByText('Ada made this')).toBeInTheDocument());

        for (const code of ['mine1', 'ada22', 'bob33']) {
            expect(screen.getByRole('button', { name: `Delete link ${code}` })).toBeInTheDocument();
        }
    });

    it('exports what the list shows: the caller\'s links in My Links, everyone\'s otherwise', async () => {
        serve();
        const { user } = await ready();
        await sharedListLoaded();
        const exports = () => vi.mocked(fetch).mock.calls
            .map(([input]) => new URL(String(input)))
            .filter(url => url.pathname === '/links/export')
            .map(url => url.searchParams);

        await user.click(screen.getByRole('button', { name: /export csv/i }));
        await waitFor(() => expect(exports()).toHaveLength(1));
        expect(exports()[0].get('org_id')).toBe(String(SHARED.id));
        expect(exports()[0].get('mine')).toBe('true');

        await user.click(option('All team links'));
        await waitFor(() => expect(lastList().has('mine')).toBe(false));
        await user.click(screen.getByRole('button', { name: /export csv/i }));
        await waitFor(() => expect(exports()).toHaveLength(2));
        expect(exports()[1].get('org_id')).toBe(String(SHARED.id));
        expect(exports()[1].has('mine')).toBe(false);
    });

    it('creates new links in the shared organization', async () => {
        serve();
        const { user } = await ready();
        await sharedListLoaded();

        expect(screen.getByText(`Creating in ${SHARED.name}`)).toBeInTheDocument();
        await user.type(screen.getByPlaceholderText(/example.com/i), 'https://example.com/new');
        await user.click(screen.getByRole('button', { name: 'Create' }));

        await waitFor(() => {
            const post = vi.mocked(fetch).mock.calls.find(([input, init]) =>
                new URL(String(input)).pathname === '/links' && init?.method === 'POST');
            expect(post).toBeDefined();
            expect(JSON.parse(String(post![1]!.body)).org_id).toBe(SHARED.id);
        });
    });
});
