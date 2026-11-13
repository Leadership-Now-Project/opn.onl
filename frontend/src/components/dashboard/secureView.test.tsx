import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, mockToken, render, screen, waitFor } from '../../test/test-utils';
import CreateFilesTab from './CreateFilesTab';
import EditModal from './EditModal';
import LinkRow from './LinkRow';
import type { AppSettings, LinkData } from './types';
import { putFile } from '../../utils/uploadFile';

// The PUT to storage is an XMLHttpRequest to another origin; everything else
// is the real component.
vi.mock('../../utils/uploadFile', async (original) => ({
    ...(await original<typeof import('../../utils/uploadFile')>()),
    putFile: vi.fn(),
}));

const EXPLANATION = 'Viewers must enter an email and see the file in a protected viewer. It deters saving and printing; it cannot stop screenshots.';
const NOT_ELIGIBLE = 'Only for PDF, PNG, JPEG, GIF and WebP files.';

const settings: AppSettings = {
    custom_aliases_enabled: false,
    min_alias_length: 4,
    max_alias_length: 50,
    qr_branding_enabled: false,
    burn_after_reading_enabled: false,
    safe_link_interstitial_enabled: false,
    conditional_routing_enabled: false,
    link_in_bio_enabled: false,
    files: { enabled: true, max_bytes: 10 * 1024 * 1024 },
};

function jsonResponse(body: unknown, status = 200): Response {
    return { ok: status >= 200 && status < 300, status, json: () => Promise.resolve(body) } as Response;
}

const theSwitch = () => screen.getByRole('switch', { name: 'Secure View Share' });

// ---------------------------------------------------------------- uploading

describe('Secure View on the Files tab', () => {
    let completed: unknown[];

    beforeEach(() => {
        completed = [];
        vi.mocked(putFile).mockReset();
        vi.mocked(putFile).mockResolvedValue(undefined);
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
        vi.mocked(global.fetch).mockReset();
        vi.mocked(global.fetch).mockImplementation(async (input, init) => {
            const url = String(input);
            if (url.endsWith('/files/usage')) return jsonResponse({ bytes_used: 0, file_count: 0, max_file_bytes: 1 });
            if (url.endsWith('/files/uploads')) {
                return jsonResponse({ upload_id: 7, upload_url: 'https://bucket.example.test/x', required_headers: {}, expires_at: '' }, 201);
            }
            completed.push(JSON.parse(String(init?.body)));
            return jsonResponse({ id: 9, code: 'pack' }, 201);
        });
    });

    function choose(name: string, type: string) {
        fireEvent.change(screen.getByLabelText('File to share'), {
            target: { files: [new File(['x'], name, { type })] },
        });
    }

    it('is greyed out until a file is chosen', () => {
        render(<CreateFilesTab appSettings={settings} active onCreated={vi.fn()} />);

        expect(theSwitch()).toBeDisabled();
        expect(theSwitch()).not.toBeChecked();
        expect(screen.getByText('Choose a file first.')).toBeInTheDocument();
    });

    it('is off for a PDF until it is switched on, and says honestly what it does', () => {
        render(<CreateFilesTab appSettings={settings} active onCreated={vi.fn()} />);
        choose('pack.pdf', 'application/pdf');

        expect(theSwitch()).toBeEnabled();
        expect(theSwitch()).not.toBeChecked();
        expect(screen.getByText(EXPLANATION)).toBeInTheDocument();
        expect(theSwitch()).toHaveAccessibleDescription(EXPLANATION);
    });

    it.each([
        ['bundle.zip', 'application/zip'],
        ['notes.docx', 'application/vnd.openxmlformats-officedocument.wordprocessingml.document'],
        ['page.html', 'application/pdf'],
        ['report.pdf', ''],
    ])('is greyed out with the reason for %s (%s)', (name, type) => {
        render(<CreateFilesTab appSettings={settings} active onCreated={vi.fn()} />);
        choose(name, type);

        expect(theSwitch()).toBeDisabled();
        expect(theSwitch()).not.toBeChecked();
        expect(screen.getByText(NOT_ELIGIBLE)).toBeInTheDocument();
        expect(screen.queryByText(EXPLANATION)).not.toBeInTheDocument();
    });

    it('sends secure_view only when it was switched on', async () => {
        render(<CreateFilesTab appSettings={settings} active onCreated={vi.fn()} />);
        choose('pack.pdf', 'application/pdf');
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));
        await waitFor(() => expect(completed).toHaveLength(1));
        expect(completed[0]).toEqual({ title: 'pack.pdf' });

        choose('chart.png', 'image/png');
        fireEvent.click(theSwitch());
        expect(theSwitch()).toBeChecked();
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));
        await waitFor(() => expect(completed).toHaveLength(2));
        expect(completed[1]).toEqual({ title: 'chart.png', secure_view: true });

        // Off again for the next file: it is never carried over.
        choose('next.pdf', 'application/pdf');
        expect(theSwitch()).not.toBeChecked();
    });

    it('goes back to off when the file is swapped for one it cannot show', async () => {
        render(<CreateFilesTab appSettings={settings} active onCreated={vi.fn()} />);
        choose('pack.pdf', 'application/pdf');
        fireEvent.click(theSwitch());
        expect(theSwitch()).toBeChecked();

        // Another file it can show: still off, it is a choice per file.
        choose('other.pdf', 'application/pdf');
        expect(theSwitch()).toBeEnabled();
        expect(theSwitch()).not.toBeChecked();
        fireEvent.click(theSwitch());

        choose('bundle.zip', 'application/zip');
        expect(theSwitch()).not.toBeChecked();
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));
        await waitFor(() => expect(completed).toHaveLength(1));
        expect(completed[0]).toEqual({ title: 'bundle.zip' });
    });
});

// ------------------------------------------------------------------ editing

const fileLink: LinkData = {
    id: 5,
    code: 'pack',
    original_url: '',
    short_url: 'https://opn.onl/pack',
    title: 'Board pack',
    click_count: 0,
    created_at: '2026-11-01T00:00:00Z',
    expires_at: null,
    has_password: false,
    notes: null,
    is_active: true,
    is_pinned: false,
    tags: [],
    file: {
        filename: 'pack.pdf',
        size_bytes: 2048,
        content_type: 'application/pdf',
        secure_view: false,
        secure_view_eligible: true,
    },
};

function withFile(file: Partial<NonNullable<LinkData['file']>>): LinkData {
    return { ...fileLink, file: { ...fileLink.file!, ...file } };
}

describe('Secure View in the edit dialog', () => {
    let order: string[];
    let sent: { url: string; method: string; body: unknown }[];

    beforeEach(() => {
        order = [];
        sent = [];
        vi.mocked(global.fetch).mockReset();
        vi.mocked(global.fetch).mockImplementation(async (input, init) => {
            order.push('secure-view');
            sent.push({ url: String(input), method: init?.method ?? 'GET', body: JSON.parse(String(init?.body)) });
            return jsonResponse({});
        });
    });

    const save = () => fireEvent.click(screen.getByRole('button', { name: /save/i }));
    const onSave = () => vi.fn().mockImplementation(async () => { order.push('save'); });

    it('switches it on before the save that reloads the list', async () => {
        const saved = onSave();
        render(<EditModal link={fileLink} onClose={vi.fn()} onSave={saved} />);

        expect(theSwitch()).toBeEnabled();
        expect(theSwitch()).not.toBeChecked();
        expect(screen.getByText(EXPLANATION)).toBeInTheDocument();
        fireEvent.click(theSwitch());
        save();

        await waitFor(() => expect(saved).toHaveBeenCalledTimes(1));
        expect(sent).toEqual([{
            url: expect.stringMatching(/\/links\/5\/secure-view$/),
            method: 'PUT',
            body: { enabled: true },
        }]);
        expect(order).toEqual(['secure-view', 'save']);
    });

    it('switches it off', async () => {
        const saved = onSave();
        render(<EditModal link={withFile({ secure_view: true })} onClose={vi.fn()} onSave={saved} />);

        expect(theSwitch()).toBeChecked();
        fireEvent.click(theSwitch());
        save();

        await waitFor(() => expect(saved).toHaveBeenCalledTimes(1));
        expect(sent.map(call => call.body)).toEqual([{ enabled: false }]);
    });

    it('sends nothing when it was left as it is', async () => {
        const saved = onSave();
        render(<EditModal link={withFile({ secure_view: true })} onClose={vi.fn()} onSave={saved} />);
        save();

        await waitFor(() => expect(saved).toHaveBeenCalledTimes(1));
        expect(sent).toEqual([]);
    });

    it('is greyed out with the reason for a file it cannot show', () => {
        render(
            <EditModal
                link={withFile({ filename: 'bundle.zip', content_type: 'application/zip', secure_view_eligible: false })}
                onClose={vi.fn()}
                onSave={vi.fn()}
            />,
        );

        expect(theSwitch()).toBeDisabled();
        expect(theSwitch()).not.toBeChecked();
        expect(screen.getByText(NOT_ELIGIBLE)).toBeInTheDocument();
    });

    it('does not save the rest when the server refuses the switch, and says why', async () => {
        vi.mocked(global.fetch).mockResolvedValue(
            jsonResponse({ error: 'Secure View works for PDF, PNG, JPEG, GIF and WebP files only' }, 400),
        );
        const saved = onSave();
        const onClose = vi.fn();
        render(<EditModal link={fileLink} onClose={onClose} onSave={saved} />);
        fireEvent.click(theSwitch());
        save();

        expect(await screen.findByText('Secure View works for PDF, PNG, JPEG, GIF and WebP files only')).toBeInTheDocument();
        expect(saved).not.toHaveBeenCalled();
        expect(onClose).not.toHaveBeenCalled();
    });

    it('is not offered on a link to a URL', () => {
        const { file: _file, ...urlLink } = fileLink;
        render(<EditModal link={{ ...urlLink, original_url: 'https://example.com/' }} onClose={vi.fn()} onSave={vi.fn()} />);

        expect(screen.queryByRole('switch', { name: 'Secure View Share' })).not.toBeInTheDocument();
    });
});

// ----------------------------------------------------------------- the list

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

describe('Secure View in the link list', () => {
    it('marks a Secure View link', () => {
        row(withFile({ secure_view: true }));
        expect(screen.getByText('Secure View')).toBeInTheDocument();
    });

    it('does not mark any other link', () => {
        const first = row(fileLink);
        expect(screen.queryByText('Secure View')).not.toBeInTheDocument();
        first.unmount();

        const { file: _file, ...urlLink } = fileLink;
        row({ ...urlLink, original_url: 'https://example.com/' });
        expect(screen.queryByText('Secure View')).not.toBeInTheDocument();
    });
});
