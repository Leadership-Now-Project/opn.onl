import { describe, it, expect, vi } from 'vitest';

// The theme is a build-time switch, read when config/theme.ts is first evaluated.
vi.hoisted(() => {
    vi.stubEnv('VITE_THEME', 'lnp');
    // The LNP images are built with their own address; the default is opn.onl's.
    vi.stubEnv('VITE_FRONTEND_URL', 'https://links.example.org');
});

import { render, waitFor } from '../test/test-utils';
import SEO from './SEO';

const meta = (selector: string) => document.head.querySelector(selector)?.getAttribute('content');

describe('SEO in the LNP build: what a share preview says', () => {
    it('names the Leadership Now Project, never opn.onl or its author', async () => {
        render(<SEO />);
        await waitFor(() => expect(meta('meta[property="og:title"]')).toBe('LNP Links'));
        expect(meta('meta[property="og:site_name"]')).toBe('Leadership Now Project');
        expect(meta('meta[property="og:description"]')).toBe('Short links from the Leadership Now Project.');
        expect(meta('meta[name="twitter:description"]')).toBe('Short links from the Leadership Now Project.');
        expect(meta('meta[name="description"]')).toBe('Short links from the Leadership Now Project.');
        expect(meta('meta[name="application-name"]')).toBe('LNP Links');
        expect(meta('meta[property="og:image"]')).toMatch(/\/og-image\.png$/);
        expect(document.head.querySelector('meta[name="twitter:creator"]')).toBeNull();
        expect(document.head.querySelector('meta[name="author"]')).toBeNull();
        // Nothing in a share tag or in structured data still says opn.onl.
        const shareTags = [
            ...document.head.querySelectorAll(
                'meta[property^="og:"], meta[name^="twitter:"], meta[name="description"], script[type="application/ld+json"]',
            ),
        ];
        expect(shareTags.length).toBeGreaterThan(5);
        for (const tag of shareTags) {
            const text = `${tag.getAttribute('content') ?? ''}${tag.textContent ?? ''}`;
            expect(text).not.toMatch(/opn\.onl|ysalitrynskyi|Salitrynskyi/);
        }
    });

    it("rewrites a page's own text that names opn.onl", async () => {
        render(<SEO title="Log in" description="Log in to opn.onl to manage your short links." />);
        await waitFor(() => expect(meta('meta[property="og:title"]')).toBe('Log in | LNP Links'));
        expect(meta('meta[property="og:description"]')).toBe('Log in to LNP Links to manage your short links.');
    });
});
