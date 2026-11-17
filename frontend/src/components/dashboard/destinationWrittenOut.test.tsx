import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '../../test/test-utils';
import { DESTINATION_NOT_WRITTEN_OUT } from '../../utils/destination';
import CreateLinkForm from './CreateLinkForm';
import EditModal from './EditModal';
import type { AppSettings, LinkData } from './types';

// A destination typed without its two slashes (`https:iana.org/x`) passes the
// browser's own `type="url"` check and parses, so the forms have to ask.

const settings: AppSettings = {
    custom_aliases_enabled: false,
    min_alias_length: 5,
    max_alias_length: 25,
    qr_branding_enabled: false,
    burn_after_reading_enabled: false,
    safe_link_interstitial_enabled: false,
    conditional_routing_enabled: false,
    link_in_bio_enabled: false,
};

const link: LinkData = {
    id: 1,
    code: 'abc_123',
    original_url: 'https://iana.org/original',
    short_url: 'https://opn.onl/abc_123',
    title: null,
    click_count: 0,
    created_at: '2026-01-01 00:00:00',
    expires_at: null,
    has_password: false,
    notes: null,
    is_active: true,
    is_pinned: false,
    tags: [],
};

describe('a destination typed without its slashes', () => {
    beforeEach(() => {
        vi.mocked(global.fetch).mockReset();
    });

    it.each(['https:iana.org/probe', 'https:/iana.org/probe', 'http:iana.org'])(
        'is not sent by the create form: %s',
        async (typed) => {
            const onError = vi.fn();
            const { user } = render(
                <CreateLinkForm appSettings={settings} initialUrl="" onCreated={vi.fn()} onError={onError} />,
            );

            await user.type(screen.getByPlaceholderText('https://example.com/long-url'), typed);
            await user.click(screen.getByRole('button', { name: 'Create' }));

            await waitFor(() => expect(onError).toHaveBeenCalledWith(DESTINATION_NOT_WRITTEN_OUT));
            expect(global.fetch).not.toHaveBeenCalled();
            // The form is usable again, with the text still there to correct.
            expect(screen.getByRole('button', { name: 'Create' })).toBeEnabled();
            expect(screen.getByPlaceholderText('https://example.com/long-url')).toHaveValue(typed);
        },
    );

    it('is sent by the create form once it is written in full, http included', async () => {
        vi.mocked(global.fetch).mockResolvedValue({ ok: true, status: 201, json: async () => ({}) } as Response);
        const onError = vi.fn();
        const { user } = render(
            <CreateLinkForm appSettings={settings} initialUrl="" onCreated={vi.fn()} onError={onError} />,
        );

        await user.type(screen.getByPlaceholderText('https://example.com/long-url'), 'http://iana.org/probe');
        await user.click(screen.getByRole('button', { name: 'Create' }));

        await waitFor(() => expect(global.fetch).toHaveBeenCalled());
        const [, init] = vi.mocked(global.fetch).mock.calls[0];
        expect(init?.body).toBe(JSON.stringify({ original_url: 'http://iana.org/probe' }));
        expect(onError).not.toHaveBeenCalledWith(DESTINATION_NOT_WRITTEN_OUT);
    });

    it('is not saved by the edit dialog', async () => {
        const onSave = vi.fn().mockResolvedValue(undefined);
        const { user } = render(<EditModal link={link} onClose={vi.fn()} onSave={onSave} />);

        fireEvent.change(screen.getByLabelText('Destination URL'), { target: { value: 'https:iana.org/new' } });
        await user.click(screen.getByRole('button', { name: /save changes/i }));

        expect(await screen.findByText(DESTINATION_NOT_WRITTEN_OUT)).toBeInTheDocument();
        expect(onSave).not.toHaveBeenCalled();
    });

    it('does not stop an older link that already has one from being saved with a new title', async () => {
        const onSave = vi.fn().mockResolvedValue(undefined);
        const { user } = render(
            <EditModal link={{ ...link, original_url: 'https:iana.org/old' }} onClose={vi.fn()} onSave={onSave} />,
        );

        fireEvent.change(screen.getByLabelText(/^title/i), { target: { value: 'A new title' } });
        await user.click(screen.getByRole('button', { name: /save changes/i }));

        await waitFor(() => expect(onSave).toHaveBeenCalledOnce());
        const payload = onSave.mock.calls[0][1];
        // The destination was not edited, so it is not sent at all.
        expect(payload.original_url).toBeUndefined();
        expect(payload.title).toBe('A new title');
    });
});
