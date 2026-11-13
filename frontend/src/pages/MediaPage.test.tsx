import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '../test/test-utils';
import MediaPage from './MediaPage';

vi.mock('react-router-dom', async () => {
    const actual = await vi.importActual('react-router-dom');
    return { ...actual, useParams: () => ({ code: 'town-hall' }) };
});

const TICKET = 'aaa.bbb.ccc';
const SRC = 'https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?start=90';

const MEDIA = {
    provider: 'youtube',
    media_id: 'dQw4w9WgXcQ',
    title: 'Town hall, November',
    cta: null as { label: string; url: string } | null,
    embed: { src: SRC },
};

interface Call { url: string; method: string; body: unknown }

function answer(status: number, body: unknown, headers: Record<string, string> = {}): Response {
    return {
        ok: status >= 200 && status < 300,
        status,
        headers: new Headers(headers),
        json: () => Promise.resolve(body),
    } as unknown as Response;
}

/** The page's one call. */
function mockApi(response: Response = answer(200, MEDIA)): Call[] {
    const calls: Call[] = [];
    vi.mocked(global.fetch).mockImplementation(async (input, init) => {
        calls.push({
            url: String(input),
            method: init?.method ?? 'GET',
            body: init?.body ? JSON.parse(String(init.body)) : undefined,
        });
        return response;
    });
    return calls;
}

/** Arrive as a visitor does: from the short link, with the ticket in the fragment. */
function arrive(fragment = `#t=${TICKET}`) {
    return render(<MediaPage />, { route: `/m/town-hall${fragment}` });
}

const player = () => document.querySelector('iframe');

describe('MediaPage', () => {
    beforeEach(() => {
        vi.mocked(global.fetch).mockReset();
    });

    it('shows the title and the video in its host\'s player, at the address the server gave', async () => {
        const calls = mockApi();
        arrive();

        expect(await screen.findByRole('heading', { name: 'Town hall, November' })).toBeVisible();
        const frame = player()!;
        expect(frame).toBeInTheDocument();
        expect(frame.getAttribute('src')).toBe(SRC);
        expect(frame).toHaveAttribute('title', 'Town hall, November');
        expect(frame).toHaveAttribute('allow', 'fullscreen; picture-in-picture; encrypted-media');
        // YouTube's player needs to be told the embedding site's origin.
        expect(frame).toHaveAttribute('referrerpolicy', 'strict-origin-when-cross-origin');
        // A 16:9 box at any width, the frame filling it.
        expect(frame.parentElement).toHaveClass('aspect-video', 'w-full');
        expect(frame).toHaveClass('absolute', 'inset-0', 'h-full', 'w-full');

        expect(calls).toHaveLength(1);
        expect(calls[0].method).toBe('POST');
        expect(calls[0].url).toMatch(/\/town-hall\/media$/);
        expect(calls[0].body).toEqual({ ticket: TICKET });
        // The ticket travels in the body, never in the address.
        expect(calls[0].url).not.toContain(TICKET);
        expect(calls[0].url).not.toContain('?');
    });

    it('keeps the ticket in the address, so that a reload works', async () => {
        mockApi();
        arrive();
        await screen.findByRole('heading', { name: 'Town hall, November' });

        expect(window.location.hash).toBe(`#t=${TICKET}`);
        expect(window.location.pathname).toBe('/m/town-hall');
    });

    it('names the player by its provider when the link has no title', async () => {
        mockApi(answer(200, { ...MEDIA, title: '', provider: 'vimeo', embed: { src: 'https://player.vimeo.com/video/76979871?h=abc123def0' } }));
        arrive();

        await waitFor(() => expect(player()).toBeInTheDocument());
        expect(player()).toHaveAttribute('title', 'Vimeo video');
        expect(player()!.getAttribute('src')).toBe('https://player.vimeo.com/video/76979871?h=abc123def0');
        expect(screen.queryByRole('heading')).not.toBeInTheDocument();
    });

    it.each([
        'https://evil.test/embed/dQw4w9WgXcQ',
        'https://www.youtube-nocookie.com.evil.test/embed/dQw4w9WgXcQ',
        'https://www.youtube-nocookie.com@evil.test/embed/x',
        'http://www.youtube-nocookie.com/embed/dQw4w9WgXcQ',
        'https://www.youtube.com/embed/dQw4w9WgXcQ',
        'javascript:alert(1)',
        'data:text/html,<script>alert(1)</script>',
        '//player.vimeo.com/video/1',
        '',
        null,
        { toString: 'https://player.vimeo.com/video/1' },
    ])('frames nothing when the answer names the address %j', async (src) => {
        mockApi(answer(200, { ...MEDIA, embed: { src } }));
        arrive();

        expect(await screen.findByText('This video cannot be shown.')).toBeVisible();
        expect(player()).not.toBeInTheDocument();
    });

    it('frames nothing when the answer has no player at all', async () => {
        mockApi(answer(200, { title: 'x' }));
        arrive();

        expect(await screen.findByText('This video cannot be shown.')).toBeVisible();
        expect(player()).not.toBeInTheDocument();
    });

    it('shows no button when the link has none', async () => {
        mockApi();
        arrive();
        await screen.findByRole('heading', { name: 'Town hall, November' });

        expect(screen.queryByRole('link')).not.toBeInTheDocument();
    });

    it('shows the sharer\'s button, opening in a new tab that cannot reach this page', async () => {
        mockApi(answer(200, { ...MEDIA, cta: { label: 'Join us', url: 'https://example.org/join' } }));
        arrive();

        const button = await screen.findByRole('link', { name: 'Join us' });
        expect(button).toHaveAttribute('href', 'https://example.org/join');
        expect(button).toHaveAttribute('target', '_blank');
        expect(button).toHaveAttribute('rel', 'noopener noreferrer');
    });

    it.each([
        'javascript:alert(document.domain)',
        'JaVaScRiPt:alert(1)',
        'data:text/html,<script>alert(1)</script>',
        'http://example.org/join',
        '//example.org/join',
        '',
    ])('shows no button for the link %j, and still shows the video', async (url) => {
        mockApi(answer(200, { ...MEDIA, cta: { label: 'Join us', url } }));
        arrive();

        await waitFor(() => expect(player()).toBeInTheDocument());
        expect(screen.queryByRole('link')).not.toBeInTheDocument();
        expect(screen.queryByText('Join us')).not.toBeInTheDocument();
    });

    it('draws a title or a label made of markup as text', async () => {
        mockApi(answer(200, {
            ...MEDIA,
            title: '<img src=x onerror=alert(1)>',
            cta: { label: '<script>alert(1)</script>', url: 'https://example.org/' },
        }));
        arrive();

        expect(await screen.findByRole('heading', { name: '<img src=x onerror=alert(1)>' })).toBeVisible();
        expect(screen.getByRole('link', { name: '<script>alert(1)</script>' })).toBeVisible();
        expect(document.querySelector('img[src="x"]')).not.toBeInTheDocument();
        expect(document.querySelector('main script, h1 script, a script')).not.toBeInTheDocument();
    });

    it('asks the server nothing without a ticket, and offers the link again', () => {
        const calls = mockApi();
        arrive('');

        expect(screen.getByRole('heading', { name: 'This page has expired' })).toBeVisible();
        const again = screen.getByRole('link', { name: 'Open the link again' });
        expect(new URL(again.getAttribute('href')!).pathname).toBe('/town-hall');
        expect(calls).toHaveLength(0);
        expect(player()).not.toBeInTheDocument();
    });

    it('says the page has expired when the server no longer takes the ticket', async () => {
        mockApi(answer(401, { error: 'This page has expired. Open the link again.' }));
        arrive();

        expect(await screen.findByRole('heading', { name: 'This page has expired' })).toBeVisible();
        const again = screen.getByRole('link', { name: 'Open the link again' });
        expect(new URL(again.getAttribute('href')!).pathname).toBe('/town-hall');
        expect(again.getAttribute('href')).not.toContain(TICKET);
        expect(player()).not.toBeInTheDocument();
    });

    it('says in plain words that a link is over', async () => {
        mockApi(answer(410, { error: 'Link has expired' }));
        arrive();

        expect(await screen.findByRole('heading', { name: 'This video is not available' })).toBeVisible();
        expect(screen.getByRole('alert')).toHaveTextContent('Link has expired.');
        expect(player()).not.toBeInTheDocument();
    });

    it('falls back to its own words when a refusal gives none', async () => {
        mockApi(answer(410, null));
        arrive();

        expect(await screen.findByRole('alert')).toHaveTextContent('This link has expired or is no longer available.');
    });

    it.each([
        [500, 'The video could not be loaded. Try again in a moment.'],
        [429, 'Too many requests. Try again in a minute.'],
    ])('says so when the server answers %i', async (status, message) => {
        mockApi(answer(status, { error: 'nope' }));
        arrive();

        expect(await screen.findByRole('alert')).toHaveTextContent(message);
        expect(screen.getByRole('link', { name: 'Open the link again' })).toBeVisible();
        expect(player()).not.toBeInTheDocument();
    });

    it('says so when the network fails', async () => {
        vi.mocked(global.fetch).mockRejectedValue(new TypeError('Failed to fetch'));
        arrive();

        expect(await screen.findByRole('alert')).toHaveTextContent('Network error. Check your connection and try again.');
    });
});
