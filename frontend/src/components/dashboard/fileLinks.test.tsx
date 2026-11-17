import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '../../test/test-utils';
import EditModal from './EditModal';
import LinkRow from './LinkRow';
import type { LinkData } from './types';

// A file link in the list and in the edit dialog: it shows its file where a
// URL link shows its destination, and offers nothing the API refuses for it.
// (The row and the dialog animate in from transparent, which jsdom reports as
// not visible, so presence is what these assert.)

const urlLink: LinkData = {
    id: 7,
    code: 'abc123',
    original_url: 'https://example.com/long',
    short_url: 'http://localhost:3000/abc123',
    title: null,
    click_count: 3,
    created_at: '2026-01-01T00:00:00Z',
    expires_at: null,
    has_password: false,
    notes: null,
    is_active: true,
    is_pinned: false,
    tags: [],
};

const fileLink: LinkData = {
    ...urlLink,
    id: 8,
    code: 'q3-report',
    original_url: '',
    title: 'Third quarter',
    file: { filename: 'Q3 report.pdf', size_bytes: 13_002_342, content_type: 'application/pdf' },
};

function row(link: LinkData) {
    return render(
        <LinkRow
            link={link}
            linkNumber={1}
            copied={false}
            sourceCopied={false}
            cloning={false}
            selected={false}
            onToggleSelect={vi.fn()}
            onCopy={vi.fn()}
            onCopySource={vi.fn()}
            onPreview={vi.fn()}
            onPin={vi.fn()}
            onClone={vi.fn()}
            onShare={vi.fn()}
            onQr={vi.fn()}
            onEdit={vi.fn()}
            onDelete={vi.fn()}
        />,
    );
}

describe('a file link in the list', () => {
    it('shows a file icon, the filename and its size where the destination goes', () => {
        row(fileLink);

        expect(screen.getByLabelText('File')).toBeInTheDocument();
        expect(screen.getByText('Q3 report.pdf')).toBeInTheDocument();
        expect(screen.getByText('12.4 MB')).toBeInTheDocument();
        // The short link is still the row's link, and its usual actions stay.
        expect(screen.getByRole('link', { name: 'localhost:3000/q3-report' })).toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Copy short URL for q3-report' })).toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Edit link q3-report' })).toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Delete link q3-report' })).toBeInTheDocument();
    });

    it('has no destination link and none of the actions that need one', () => {
        row(fileLink);

        // The only links are the short URL, its API form and analytics: none is empty.
        for (const link of screen.getAllByRole('link')) {
            expect(link.getAttribute('href')).toBeTruthy();
        }
        expect(screen.queryByRole('button', { name: /Copy source URL/ })).not.toBeInTheDocument();
        expect(screen.queryByRole('button', { name: /Preview destination/ })).not.toBeInTheDocument();
        expect(screen.queryByRole('button', { name: /Clone link/ })).not.toBeInTheDocument();
    });

    it('leaves a URL link exactly as it was', () => {
        row(urlLink);

        expect(screen.getByRole('link', { name: 'example.com/long' })).toHaveAttribute('href', 'https://example.com/long');
        expect(screen.getByRole('button', { name: 'Copy source URL for abc123' })).toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Preview destination for abc123' })).toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Clone link abc123' })).toBeInTheDocument();
        expect(screen.queryByLabelText('File')).not.toBeInTheDocument();
    });
});

describe('editing a file link', () => {
    beforeEach(() => {
        vi.mocked(global.fetch).mockReset();
    });

    it('shows the file, hides the destination and routing, and never sends a URL', async () => {
        const onSave = vi.fn().mockResolvedValue(undefined);
        render(<EditModal link={fileLink} onClose={vi.fn()} onSave={onSave} routingEnabled />);

        expect(screen.getByText('Q3 report.pdf')).toBeInTheDocument();
        expect(screen.getByText('(12.4 MB)')).toBeInTheDocument();
        expect(screen.getByLabelText('Destination URL').closest('[hidden]')).not.toBeNull();
        expect(screen.queryByRole('button', { name: /smart routing/i })).not.toBeInTheDocument();
        // Routing rules are not even asked for.
        expect(global.fetch).not.toHaveBeenCalled();

        fireEvent.change(screen.getByLabelText(/^title/i), { target: { value: 'Renamed' } });
        fireEvent.click(screen.getByRole('button', { name: /save/i }));

        await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
        const payload = onSave.mock.calls[0][1] ?? onSave.mock.calls[0][0];
        expect(payload.title).toBe('Renamed');
        expect(payload.original_url).toBeUndefined();
    });
});
