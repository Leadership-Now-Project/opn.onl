import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mockToken, render, screen, within } from '../test/test-utils';
import Analytics from './Analytics';

vi.mock('react-router-dom', async () => {
    const actual = await vi.importActual('react-router-dom');
    return { ...actual, useParams: () => ({ id: '1' }), useNavigate: () => vi.fn() };
});

const click = (id: number, viewer_email?: string) => ({
    id,
    timestamp: `2026-11-13T12:0${id}:00Z`,
    country: 'United States',
    city: 'New York',
    device: 'Desktop',
    browser: 'Chrome',
    os: 'Windows',
    referer: null,
    ...(viewer_email ? { viewer_email } : {}),
});

function serve(recent_clicks: unknown[]) {
    global.fetch = vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        json: () => Promise.resolve({
            link_id: 1,
            code: 'pack',
            original_url: '',
            total_clicks: recent_clicks.length,
            unique_visitors: 1,
            clicks_by_day: [],
            clicks_by_country: [],
            clicks_by_city: [],
            clicks_by_device: [],
            clicks_by_browser: [],
            clicks_by_os: [],
            clicks_by_referer: [],
            clicks_by_destination: [],
            geo_data: [],
            recent_clicks,
        }),
    });
}

async function recentClicks() {
    const heading = await screen.findByRole('heading', { name: 'Recent clicks' });
    return within(heading.closest('div')!.parentElement!);
}

// The API sends `viewer_email` only to the link's creator and its
// organization's admins. The page shows what it is sent, and nothing more.
describe('Analytics: who viewed a Secure View link', () => {
    beforeEach(() => {
        vi.clearAllMocks();
        localStorage.setItem('token', mockToken);
    });

    it('lists the emails when the API sends them', async () => {
        serve([click(1, 'pat@example.org'), click(2), click(3, 'sam@example.org')]);
        render(<Analytics />);
        const table = await recentClicks();

        expect(table.getByRole('columnheader', { name: 'Viewer email' })).toBeInTheDocument();
        expect(table.getByText('pat@example.org')).toBeInTheDocument();
        expect(table.getByText('sam@example.org')).toBeInTheDocument();
        // A click without one keeps its row.
        expect(table.getAllByRole('row')).toHaveLength(4);
        expect(table.getByText(/not verified, and only you and your organization's admins see them/)).toBeInTheDocument();
    });

    it('lists every viewer the API sent, not only the latest twenty', async () => {
        serve(Array.from({ length: 30 }, (_, index) => ({ ...click(1, `reader${index}@example.org`), id: index + 1 })));
        render(<Analytics />);
        const table = await recentClicks();

        expect(table.getAllByRole('row')).toHaveLength(31);
        expect(table.getByText('reader29@example.org')).toBeInTheDocument();
    });

    it('keeps the list to twenty when there are no viewer emails', async () => {
        serve(Array.from({ length: 30 }, (_, index) => ({ ...click(1), id: index + 1 })));
        render(<Analytics />);
        const table = await recentClicks();

        expect(table.getAllByRole('row')).toHaveLength(21);
    });

    it('has no email column when the API sends none', async () => {
        serve([click(1), click(2)]);
        render(<Analytics />);
        const table = await recentClicks();

        expect(table.queryByRole('columnheader', { name: 'Viewer email' })).not.toBeInTheDocument();
        expect(table.getAllByRole('columnheader')).toHaveLength(5);
        expect(screen.queryByText(/not verified/)).not.toBeInTheDocument();
    });
});
