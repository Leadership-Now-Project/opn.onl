import { act } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, mockToken, render, screen, waitFor } from '../../test/test-utils';
import CreateFilesTab from './CreateFilesTab';
import type { AppSettings } from './types';
import { localIsoDate } from '../../utils/localIsoDate';
import { putFile, UploadCancelled } from '../../utils/uploadFile';

// The PUT to storage is the one call a test cannot let through: it is an
// XMLHttpRequest to another origin. Everything else is the real component.
vi.mock('../../utils/uploadFile', async (original) => ({
    ...(await original<typeof import('../../utils/uploadFile')>()),
    putFile: vi.fn(),
}));

const MB = 1024 * 1024;

const settings: AppSettings = {
    custom_aliases_enabled: true,
    min_alias_length: 4,
    max_alias_length: 50,
    qr_branding_enabled: false,
    burn_after_reading_enabled: false,
    safe_link_interstitial_enabled: false,
    conditional_routing_enabled: false,
    link_in_bio_enabled: false,
    files: { enabled: true, max_bytes: 10 * MB },
};

function jsonResponse(body: unknown, status = 200): Response {
    return {
        ok: status >= 200 && status < 300,
        status,
        json: () => Promise.resolve(body),
    } as Response;
}

const STARTED = {
    upload_id: 42,
    upload_url: 'https://bucket.example.test/files/abc?X-Amz-Signature=s',
    required_headers: { 'content-type': 'application/pdf' },
    expires_at: '2026-11-13T12:15:00Z',
};

interface Calls {
    start: unknown[];
    complete: { url: string; body: unknown }[];
    usage: number;
}

/** The API as the tab uses it. `overrides` replaces one answer. */
function mockApi(overrides: { start?: Response; complete?: Response; usage?: Response } = {}): Calls {
    const calls: Calls = { start: [], complete: [], usage: 0 };
    vi.mocked(global.fetch).mockImplementation(async (input, init) => {
        const url = String(input);
        if (url.endsWith('/files/usage')) {
            calls.usage += 1;
            return overrides.usage ?? jsonResponse({ bytes_used: 13_002_342, file_count: 3, max_file_bytes: 10 * MB });
        }
        if (url.endsWith('/files/uploads')) {
            calls.start.push(JSON.parse(String(init?.body)));
            return overrides.start ?? jsonResponse(STARTED, 201);
        }
        if (/\/files\/uploads\/\d+\/complete$/.test(url)) {
            calls.complete.push({ url, body: JSON.parse(String(init?.body)) });
            return overrides.complete ?? jsonResponse({ id: 9, code: 'q3-report' }, 201);
        }
        return jsonResponse({ available: true, code: '', message: '' });
    });
    return calls;
}

function renderTab(appSettings: AppSettings = settings, extra: { active?: boolean; orgId?: number | null; orgName?: string | null } = {}) {
    const onCreated = vi.fn();
    const view = render(
        <CreateFilesTab appSettings={appSettings} active={extra.active ?? true} onCreated={onCreated} orgId={extra.orgId} orgName={extra.orgName} />,
    );
    return { ...view, onCreated };
}

function pdf(name = 'Q3 report.pdf', size = 2 * MB): File {
    const file = new File(['x'], name, { type: 'application/pdf' });
    Object.defineProperty(file, 'size', { value: size });
    return file;
}

function chooseFile(file: File) {
    fireEvent.change(screen.getByLabelText('File to share'), { target: { files: [file] } });
}

describe('CreateFilesTab', () => {
    beforeEach(() => {
        vi.mocked(global.fetch).mockReset();
        vi.mocked(putFile).mockReset();
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
    });

    it('says file sharing is not set up when the server names no file storage', () => {
        mockApi();
        const { files: _files, ...withoutFiles } = settings;
        renderTab(withoutFiles);

        expect(screen.getByText('File sharing is not set up on this server.')).toBeVisible();
        expect(screen.queryByText('Upload a File')).not.toBeInTheDocument();
        expect(screen.queryByRole('button', { name: /Google Drive/ })).not.toBeInTheDocument();
        expect(global.fetch).not.toHaveBeenCalled();
    });

    it('offers the two providers, both disabled on a server without the Drive picker, and the upload', async () => {
        mockApi();
        renderTab();

        const drive = screen.getByRole('button', { name: /Google Drive Files/ });
        const oneDrive = screen.getByRole('button', { name: /Microsoft OneDrive Files/ });
        expect(drive).toBeDisabled();
        expect(drive).toHaveTextContent('Not set up on this server');
        expect(oneDrive).toBeDisabled();
        expect(oneDrive).toHaveTextContent('Coming soon');
        expect(screen.getByRole('heading', { name: 'Upload a File' })).toBeVisible();
        expect(screen.getByText('Any type, up to 10 MB')).toBeVisible();
        // Idle: nothing to create yet, and no link fields until a file is chosen.
        expect(screen.getByRole('button', { name: 'Create' })).toBeDisabled();
        expect(screen.queryByLabelText('Title')).not.toBeInTheDocument();
        await waitFor(() => expect(screen.getByText('Your uploads: 12.4 MB in 3 files')).toBeVisible());
        // Nothing here loads a third-party script.
        expect(document.querySelector('script[src*="google"], script[src*="microsoft"], script[src*="live.com"]')).toBeNull();
    });

    it('asks for storage use only once the tab is the one showing', async () => {
        const calls = mockApi({ usage: jsonResponse({ bytes_used: 500, file_count: 1, max_file_bytes: 10 * MB }) });
        const view = renderTab(settings, { active: false });
        await act(async () => {});
        expect(calls.usage).toBe(0);

        view.rerender(<CreateFilesTab appSettings={settings} active onCreated={vi.fn()} />);
        await waitFor(() => expect(screen.getByText('Your uploads: 500 bytes in 1 file')).toBeVisible());
        expect(calls.usage).toBe(1);
    });

    it('refuses a file over the limit before anything is uploaded', async () => {
        const calls = mockApi();
        renderTab();

        chooseFile(pdf('huge.zip', 10 * MB + 1));

        expect(screen.getByRole('alert')).toHaveTextContent(
            'This file is too large. The largest file you can share is 10 MB.',
        );
        expect(screen.getByRole('button', { name: 'Create' })).toBeDisabled();
        expect(screen.queryByLabelText('Title')).not.toBeInTheDocument();
        fireEvent.submit(screen.getByRole('button', { name: 'Create' }).closest('form')!);
        await act(async () => {});
        expect(calls.start).toEqual([]);
        expect(putFile).not.toHaveBeenCalled();

        // A file exactly at the limit is fine.
        chooseFile(pdf('fits.zip', 10 * MB));
        expect(screen.queryByRole('alert')).not.toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Create' })).toBeEnabled();
    });

    it('titles the link after the file until the title is typed', () => {
        mockApi();
        renderTab();

        chooseFile(pdf('Q3 report.pdf'));
        expect(screen.getByLabelText('Title')).toHaveValue('Q3 report.pdf');
        expect(screen.getByLabelText('Expires (optional)')).toHaveValue('');
        expect(screen.getByText('2 MB')).toBeVisible();

        fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'Board pack' } });
        chooseFile(pdf('other.pdf'));
        expect(screen.getByLabelText('Title')).toHaveValue('Board pack');
    });

    it('offers no expiry date before today, like the other expiry fields', () => {
        mockApi();
        renderTab();
        chooseFile(pdf('Q3 report.pdf'));

        // A file link that is already expired loses its file at the next sweep.
        expect(screen.getByLabelText('Expires (optional)')).toHaveAttribute('min', localIsoDate());
    });

    it('uploads with progress, then creates the link and reports it', async () => {
        const calls = mockApi();
        let report: (fraction: number) => void = () => {};
        let finish: () => void = () => {};
        vi.mocked(putFile).mockImplementation((_url, _headers, _file, onProgress) => {
            report = onProgress;
            return new Promise<void>(resolve => { finish = resolve; });
        });
        const { onCreated } = renderTab(settings, { orgId: 7, orgName: 'Leadership Now Project' });
        expect(screen.getByText('Creating in Leadership Now Project')).toBeVisible();

        const file = pdf('Q3 report.pdf');
        chooseFile(file);
        fireEvent.change(screen.getByLabelText('Custom alias'), { target: { value: 'q3-report' } });
        fireEvent.change(screen.getByLabelText('Expires (optional)'), { target: { value: '2027-01-31' } });
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));

        // Uploading: a progress bar that follows the upload, and a way out.
        const bar = await screen.findByRole('progressbar', { name: 'Upload progress' });
        expect(bar).toHaveAttribute('aria-valuenow', '0');
        await waitFor(() => expect(putFile).toHaveBeenCalledTimes(1));
        expect(calls.start).toEqual([{ filename: 'Q3 report.pdf', content_type: 'application/pdf', size_bytes: 2 * MB }]);
        const [url, headers, sent] = vi.mocked(putFile).mock.calls[0];
        expect(url).toBe(STARTED.upload_url);
        expect(headers).toEqual({ 'content-type': 'application/pdf' });
        expect(sent).toBe(file);
        act(() => report(0.42));
        expect(bar).toHaveAttribute('aria-valuenow', '42');
        expect(screen.getByText('Uploading… 42%')).toBeVisible();
        expect(screen.getByRole('button', { name: 'Cancel' })).toBeVisible();
        expect(screen.getByRole('button', { name: 'Uploading…' })).toBeDisabled();
        expect(calls.complete).toEqual([]);

        // Done: the link is made from the upload, with the fields typed.
        await act(async () => { finish(); });
        await waitFor(() => expect(onCreated).toHaveBeenCalledTimes(1));
        expect(calls.complete).toHaveLength(1);
        expect(calls.complete[0].url).toMatch(/\/files\/uploads\/42\/complete$/);
        expect(calls.complete[0].body).toEqual({
            custom_alias: 'q3-report',
            title: 'Q3 report.pdf',
            expires_at: new Date('2027-01-31T23:59:00').toISOString(),
            org_id: 7,
        });
        expect(screen.getByRole('status')).toHaveTextContent('File shared: localhost:3000/q3-report');
        expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
        expect(screen.queryByLabelText('Title')).not.toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'Create' })).toBeDisabled();
        // Storage use is read again now that there is one more file.
        await waitFor(() => expect(calls.usage).toBe(2));
    });

    it('sends no expiry and no alias when none was given', async () => {
        const calls = mockApi();
        vi.mocked(putFile).mockResolvedValue();
        const { onCreated } = renderTab();

        chooseFile(pdf('notes.pdf'));
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));

        await waitFor(() => expect(onCreated).toHaveBeenCalled());
        expect(calls.complete[0].body).toEqual({ title: 'notes.pdf' });
    });

    it('shows why the server would not start the upload, and keeps the file', async () => {
        mockApi({ start: jsonResponse({ error: 'The file is larger than the 1000 byte limit' }, 413) });
        const { onCreated } = renderTab();

        chooseFile(pdf());
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));

        expect(await screen.findByRole('alert')).toHaveTextContent('The file is larger than the 1000 byte limit');
        expect(putFile).not.toHaveBeenCalled();
        expect(onCreated).not.toHaveBeenCalled();
        expect(screen.getByLabelText('Title')).toHaveValue('Q3 report.pdf');
        expect(screen.getByRole('button', { name: 'Create' })).toBeEnabled();
    });

    it('reports an upload that storage refused, without creating a link', async () => {
        const calls = mockApi();
        vi.mocked(putFile).mockRejectedValue(new Error('Storage refused the upload (403)'));
        const { onCreated } = renderTab();

        chooseFile(pdf());
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));

        expect(await screen.findByRole('alert')).toHaveTextContent(
            'The upload failed. Check your connection and try again.',
        );
        expect(calls.complete).toEqual([]);
        expect(onCreated).not.toHaveBeenCalled();
        expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    });

    it('reports a link the server refused after the upload', async () => {
        mockApi({ complete: jsonResponse({ error: 'Alias already taken' }, 409) });
        vi.mocked(putFile).mockResolvedValue();
        const { onCreated } = renderTab();

        chooseFile(pdf());
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));

        expect(await screen.findByRole('alert')).toHaveTextContent('Alias already taken');
        expect(onCreated).not.toHaveBeenCalled();
        expect(screen.queryByRole('status')).not.toBeInTheDocument();
    });

    it('stops quietly when the upload is cancelled', async () => {
        const calls = mockApi();
        let signal: AbortSignal | undefined;
        vi.mocked(putFile).mockImplementation((_url, _headers, _file, _onProgress, given) => {
            signal = given;
            return new Promise<void>((_resolve, reject) => {
                given.addEventListener('abort', () => reject(new UploadCancelled()));
            });
        });
        const { onCreated } = renderTab();

        chooseFile(pdf());
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));
        fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }));

        await waitFor(() => expect(screen.queryByRole('progressbar')).not.toBeInTheDocument());
        expect(signal?.aborted).toBe(true);
        expect(screen.queryByRole('alert')).not.toBeInTheDocument();
        expect(calls.complete).toEqual([]);
        expect(onCreated).not.toHaveBeenCalled();
        // The file is still chosen, ready to try again.
        expect(screen.getByRole('button', { name: 'Create' })).toBeEnabled();
    });

    it('checks the alias before uploading anything', async () => {
        const calls = mockApi();
        renderTab();

        chooseFile(pdf());
        fireEvent.change(screen.getByLabelText('Custom alias'), { target: { value: 'ab' } });
        fireEvent.click(screen.getByRole('button', { name: 'Create' }));

        expect(await screen.findByRole('alert')).toHaveTextContent('Alias must be at least 4 characters');
        expect(calls.start).toEqual([]);
    });
});
