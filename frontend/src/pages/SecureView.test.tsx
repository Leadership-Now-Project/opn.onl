import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '../test/test-utils';
import SecureView from './SecureView';
import { openPdf } from '../components/viewer/pdfDocument';
import { drawWatermark } from '../components/viewer/watermark';

vi.mock('react-router-dom', async () => {
    const actual = await vi.importActual('react-router-dom');
    return { ...actual, useParams: () => ({ code: 'board-pack' }) };
});

// PDF.js needs a real canvas and a worker, which jsdom has neither of. This
// is the one file that imports it; everything else here is the real viewer.
vi.mock('../components/viewer/pdfDocument', () => ({ openPdf: vi.fn() }));

// The text is real; only the drawing is watched (jsdom has no 2D context).
vi.mock('../components/viewer/watermark', async (original) => ({
    ...(await original<typeof import('../components/viewer/watermark')>()),
    drawWatermark: vi.fn(),
}));

const TICKET = 'aaa.bbb.ccc';
const TOKEN = 'ddd.eee.fff';

const VIEW = {
    view_token: TOKEN,
    expires_in: 300,
    title: 'Board pack, November',
    file: { kind: 'pdf', content_type: 'application/pdf', filename: 'pack.pdf', size_bytes: 4 },
    watermark: { email: 'pat@example.org', at: '2026-11-13T14:05:42Z' },
};

interface Call {
    url: string;
    method: string;
    headers: Record<string, string>;
    body: unknown;
}

function answer(status: number, body: unknown, headers: Record<string, string> = {}): Response {
    return {
        ok: status >= 200 && status < 300,
        status,
        headers: new Headers(headers),
        json: () => Promise.resolve(body),
        arrayBuffer: () => Promise.resolve(new Uint8Array([37, 80, 68, 70]).buffer),
    } as unknown as Response;
}

/** The two calls the viewer makes. `open` and `file` replace the answers. */
function mockApi(overrides: { open?: Response; file?: Response } = {}): Call[] {
    const calls: Call[] = [];
    vi.mocked(global.fetch).mockImplementation(async (input, init) => {
        const url = String(input);
        calls.push({
            url,
            method: init?.method ?? 'GET',
            headers: (init?.headers ?? {}) as Record<string, string>,
            body: init?.body ? JSON.parse(String(init.body)) : undefined,
        });
        if (url.endsWith('/view/file')) return overrides.file ?? answer(200, null);
        return overrides.open ?? answer(200, VIEW);
    });
    return calls;
}

function mockPdf(pageCount = 2) {
    const handle = {
        pageCount,
        pageSize: vi.fn().mockResolvedValue({ width: 600, height: 800 }),
        renderPage: vi.fn().mockResolvedValue(undefined),
        destroy: vi.fn().mockResolvedValue(undefined),
    };
    vi.mocked(openPdf).mockResolvedValue(handle);
    return handle;
}

/** Arrive as a visitor does: from the short link, with the ticket in the fragment. */
function arrive(fragment = `#t=${TICKET}`) {
    return render(<SecureView />, { route: `/v/board-pack${fragment}` });
}

async function giveEmail(user: ReturnType<typeof render>['user'], email = 'Pat@Example.org') {
    await user.type(screen.getByLabelText('Email address'), email);
    await user.click(screen.getByRole('button', { name: 'View document' }));
}

describe('SecureView', () => {
    beforeEach(() => {
        vi.mocked(global.fetch).mockReset();
        vi.mocked(openPdf).mockReset();
        vi.mocked(drawWatermark).mockReset();
    });

    it('asks for an email before it asks the server for anything', () => {
        const calls = mockApi();
        mockPdf();
        arrive();

        expect(screen.getByRole('heading', { name: 'Enter your email to view this document' })).toBeVisible();
        expect(screen.getByText(/shown on the document and is visible to the person who shared it/)).toBeVisible();
        expect(screen.getByRole('button', { name: 'View document' })).toBeDisabled();
        expect(calls).toHaveLength(0);
        expect(openPdf).not.toHaveBeenCalled();
        expect(screen.queryByTestId('protected-document')).not.toBeInTheDocument();
    });

    it('takes the ticket out of the address bar', () => {
        mockApi();
        arrive();

        expect(window.location.hash).toBe('');
        expect(window.location.pathname).toBe('/v/board-pack');
        expect(window.location.href).not.toContain(TICKET);
    });

    it('sends the ticket and the email, then fetches the file with the token in a header and never in the address', async () => {
        const calls = mockApi();
        const pdf = mockPdf();
        const { user } = arrive();
        await giveEmail(user);

        await waitFor(() => expect(openPdf).toHaveBeenCalledTimes(1));
        expect(calls).toHaveLength(2);
        const [open, file] = calls;
        expect(open.method).toBe('POST');
        expect(open.url).toMatch(/\/board-pack\/view$/);
        expect(open.body).toEqual({ ticket: TICKET, email: 'Pat@Example.org' });
        expect(open.url).not.toContain(TICKET);

        expect(file.method).toBe('GET');
        expect(file.url).toMatch(/\/board-pack\/view\/file$/);
        expect(file.headers).toEqual({ 'X-View-Token': TOKEN });
        expect(file.url).not.toContain(TOKEN);
        expect(file.url).not.toContain('?');
        for (const call of calls) expect(call.url).not.toContain(TOKEN);
        expect(window.location.href).not.toContain(TOKEN);

        // The bytes go to PDF.js as data: it is given no address to fetch.
        const [data] = vi.mocked(openPdf).mock.calls[0];
        expect(data).toBeInstanceOf(Uint8Array);
        expect(Array.from(data)).toEqual([37, 80, 68, 70]);

        expect(await screen.findByRole('heading', { name: 'Board pack, November' })).toBeVisible();
        await waitFor(() => expect(pdf.renderPage).toHaveBeenCalledTimes(2));
    });

    it('draws the email and the time into every page and lays them over the document', async () => {
        mockApi();
        const pdf = mockPdf(3);
        const { user } = arrive();
        await giveEmail(user);

        const mark = 'pat@example.org · 2026-11-13 14:05 UTC';
        await waitFor(() => expect(drawWatermark).toHaveBeenCalledTimes(3));
        for (const [canvas, text] of vi.mocked(drawWatermark).mock.calls) {
            expect(canvas).toBeInstanceOf(HTMLCanvasElement);
            expect(text).toBe(mark);
        }
        // Each page's watermark goes on after that page is drawn.
        const drawn = pdf.renderPage.mock.invocationCallOrder;
        const marked = vi.mocked(drawWatermark).mock.invocationCallOrder;
        expect(Math.min(...marked)).toBeGreaterThan(Math.min(...drawn));

        const overlay = screen.getByTestId('watermark-overlay');
        expect(overlay).toHaveAttribute('data-watermark', mark);
        expect(decodeURIComponent(overlay.style.backgroundImage)).toContain(mark);
        expect(overlay).toHaveClass('pointer-events-none');
        expect(screen.getByText(/Viewing as pat@example.org/)).toBeVisible();
    });

    it('shows a PDF as canvases with nothing to select, download, print or open', async () => {
        mockApi();
        mockPdf(2);
        const { user, container } = arrive();
        await giveEmail(user);
        await screen.findByTestId('protected-document');

        expect(container.querySelectorAll('canvas')).toHaveLength(2);
        expect(screen.queryByRole('link', { name: /download|print|open|save/i })).not.toBeInTheDocument();
        expect(screen.queryByRole('button', { name: /download|print|open|save/i })).not.toBeInTheDocument();
        expect(container.querySelector('a[download]')).toBeNull();
        expect(container.querySelector('img, iframe, embed, object')).toBeNull();
        expect(container.querySelector('.textLayer')).toBeNull();
        // No link anywhere in the viewer leads to the file.
        for (const link of container.querySelectorAll('a')) {
            expect(link.getAttribute('href') ?? '').not.toMatch(/view\/file|blob:/);
        }
    });

    it('draws an image on a canvas, not in an img, and lets its object URL go', async () => {
        const created: string[] = [];
        const revoked: string[] = [];
        URL.createObjectURL = vi.fn(() => { created.push('blob:picture'); return 'blob:picture'; });
        URL.revokeObjectURL = vi.fn((url: string) => { revoked.push(url); });
        mockApi({ open: answer(200, { ...VIEW, file: { ...VIEW.file, kind: 'image', content_type: 'image/png' } }) });
        const { user, container, unmount } = arrive();
        await giveEmail(user);
        await screen.findByTestId('protected-document');

        expect(openPdf).not.toHaveBeenCalled();
        expect(container.querySelectorAll('canvas')).toHaveLength(1);
        expect(container.querySelector('img')).toBeNull();
        expect(created).toHaveLength(1);
        unmount();
        expect(revoked).toContain('blob:picture');
    });

    it('stops PDF.js when the page is left', async () => {
        mockApi();
        const pdf = mockPdf();
        const { user, unmount } = arrive();
        await giveEmail(user);
        await screen.findByTestId('protected-document');

        expect(pdf.destroy).not.toHaveBeenCalled();
        unmount();
        expect(pdf.destroy).toHaveBeenCalledTimes(1);
    });

    it('without a ticket it offers to open the link again and asks the server nothing', () => {
        const calls = mockApi();
        arrive('');

        expect(screen.getByRole('heading', { name: 'This viewing session has expired' })).toBeVisible();
        const again = screen.getByRole('link', { name: 'Open the link again' });
        expect(again.getAttribute('href')).toMatch(/\/board-pack$/);
        expect(again.getAttribute('href')).not.toContain('/v/');
        expect(screen.queryByLabelText('Email address')).not.toBeInTheDocument();
        expect(calls).toHaveLength(0);
    });

    it.each([
        ['an expired ticket', { open: answer(401, { error: 'This viewing session has expired. Open the link again.' }) }],
        ['a token the server no longer takes', { file: answer(401, { error: 'x' }) }],
    ])('says the session expired for %s, with the way back', async (_what, overrides) => {
        mockApi(overrides);
        mockPdf();
        const { user } = arrive();
        await giveEmail(user);

        expect(await screen.findByRole('heading', { name: 'This viewing session has expired' })).toBeVisible();
        expect(screen.getByRole('link', { name: 'Open the link again' })).toBeVisible();
        expect(screen.queryByTestId('protected-document')).not.toBeInTheDocument();
    });

    it.each([
        ['Link has expired', 'Link has expired.'],
        ['This one-time link has already been opened', 'This one-time link has already been opened.'],
        ['This file is no longer available', 'This file is no longer available.'],
        [undefined, 'This link is no longer available.'],
    ])('says why a link is over: %s', async (reason, shown) => {
        mockApi({ open: answer(410, reason ? { error: reason } : {}) });
        const { user } = arrive();
        await giveEmail(user);

        expect(await screen.findByRole('heading', { name: 'This document is not available' })).toBeVisible();
        expect(screen.getByRole('alert')).toHaveTextContent(shown);
        expect(screen.queryByLabelText('Email address')).not.toBeInTheDocument();
    });

    it('keeps the form for an email the server refuses, and for too many attempts', async () => {
        const calls = mockApi({ open: answer(400, { error: 'Enter a valid email address' }) });
        const { user } = arrive();
        await giveEmail(user, 'pat@nowhere');

        expect(await screen.findByRole('alert')).toHaveTextContent('Enter a valid email address.');
        expect(screen.getByLabelText('Email address')).toHaveValue('pat@nowhere');
        expect(calls).toHaveLength(1);

        mockApi({ open: answer(429, { error: 'Too many requests' }, { 'Retry-After': '37' }) });
        await user.click(screen.getByRole('button', { name: 'View document' }));
        await waitFor(() => {
            expect(screen.getByRole('alert')).toHaveTextContent('Too many attempts. Try again in 37 seconds.');
        });
        expect(screen.getByLabelText('Email address')).toBeVisible();
    });

    it('says so when the document cannot be loaded or drawn', async () => {
        mockApi({ file: answer(502, { error: 'File storage is unavailable' }) });
        const { user } = arrive();
        await giveEmail(user);
        expect(await screen.findByRole('alert')).toHaveTextContent('The document could not be loaded.');

        fireEvent.click(document.body);
        expect(screen.queryByTestId('protected-document')).not.toBeInTheDocument();
    });
});
