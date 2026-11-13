import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, mockToken, render, screen, waitFor, within } from '../../test/test-utils';
import { driveFile, driveJson, fakeGoogle, type DriveCall, type FakeGoogleOptions } from '../../test/fakeGoogle';
import { forgetDriveToken } from '../../utils/googleDrive';
import CreateMediaTab from './CreateMediaTab';
import EditModal from './EditModal';
import LinkRow from './LinkRow';
import type { AppSettings, LinkData } from './types';

// The Media tab: the real tab, the real Drive panel and the real Drive module,
// with Google replaced by `fakeGoogle`. `global.fetch` is our own API.

const settings: AppSettings = {
    custom_aliases_enabled: true,
    min_alias_length: 4,
    max_alias_length: 50,
    qr_branding_enabled: false,
    burn_after_reading_enabled: false,
    safe_link_interstitial_enabled: false,
    conditional_routing_enabled: false,
    link_in_bio_enabled: false,
    google_client_id: 'client-1.apps.googleusercontent.com',
    drive: { api_key: 'browser-key', app_id: '1234567890' },
};

const VIDEO = {
    id: 'video-1',
    name: 'Town hall.mp4',
    mimeType: 'video/mp4',
    webViewLink: 'https://drive.google.com/file/d/video-1/view?usp=drivesdk',
};
const PUBLIC = [{ type: 'user', role: 'owner' }, { type: 'anyone', role: 'reader' }];

interface ApiCall { url: string; method: string; body: Record<string, unknown> | undefined }
interface ApiAnswer { status: number; body: unknown }

function setUp(options: {
    links?: ApiAnswer;
    file?: Record<string, unknown>;
    google?: FakeGoogleOptions;
    appSettings?: AppSettings;
    orgId?: number | null;
    orgName?: string | null;
} = {}) {
    const order: string[] = [];
    const apiCalls: ApiCall[] = [];
    const google = fakeGoogle({
        pick: 'video-1',
        drive: (call: DriveCall) => call.method === 'POST'
            ? driveJson({ id: 'anyoneWithLink' })
            : driveJson(driveFile({ ...VIDEO, ...options.file })),
        onDriveCall: (call) => order.push(`drive ${call.method}`),
        ...options.google,
    });
    vi.mocked(global.fetch).mockImplementation(async (input, init) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        apiCalls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
        if (method === 'POST' && url.endsWith('/links')) {
            order.push('api POST /links');
            const answer = options.links ?? { status: 201, body: { id: 5, code: 'town-hall', media: { provider: 'youtube', embed_check: 'checked' } } };
            return { ok: answer.status < 300, status: answer.status, json: () => Promise.resolve(answer.body) } as Response;
        }
        // The live alias check.
        return { ok: true, status: 200, json: () => Promise.resolve({ available: true, code: '', message: '' }) } as Response;
    });
    const onCreated = vi.fn();
    const view = render(
        <CreateMediaTab
            appSettings={options.appSettings ?? settings}
            onCreated={onCreated}
            orgId={options.orgId}
            orgName={options.orgName}
            driveEnv={google.env}
        />,
    );
    const created = () => apiCalls.filter(call => call.method === 'POST' && call.url.endsWith('/links'));
    const permissionPosts = () => google.driveCalls.filter(call => call.method === 'POST');
    return { ...view, google, order, apiCalls, onCreated, created, permissionPosts };
}

const address = () => screen.getByLabelText('YouTube or Vimeo address');
const hint = () => document.getElementById('media-url-hint')!;
const type = (value: string) => fireEvent.change(address(), { target: { value } });
const videoForm = () => screen.getByRole('form', { name: 'Share a YouTube or Vimeo video' });
const create = () => fireEvent.submit(videoForm());
const driveButton = () => screen.getByRole('button', { name: /Google Drive video/ });
const settled = () => new Promise(resolve => setTimeout(resolve, 30));

async function pickVideo() {
    fireEvent.click(driveButton());
    return screen.findByRole('form', { name: 'Share a Google Drive video' });
}

describe('the Media tab: a YouTube or Vimeo address', () => {
    beforeEach(() => {
        forgetDriveToken();
        vi.mocked(global.fetch).mockReset();
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
    });

    it('says what the address is recognised as while it is typed', () => {
        setUp();
        expect(hint()).toHaveTextContent('Paste the address of one video.');
        expect(within(videoForm()).getByRole('button', { name: 'Create' })).toBeDisabled();

        type('https://youtu.be/dQw4w9WgXcQ?t=42');
        expect(hint()).toHaveTextContent('Recognised as a YouTube video.');
        expect(within(videoForm()).getByRole('button', { name: 'Create' })).toBeEnabled();

        type('https://vimeo.com/76979871/abc123def0');
        expect(hint()).toHaveTextContent('Recognised as a Vimeo video.');

        for (const other of ['https://example.org/video', 'https://youtube.com.evil.test/watch?v=dQw4w9WgXcQ', 'https://www.youtube.com/playlist?list=PL1']) {
            type(other);
            expect(hint()).toHaveTextContent('Not recognised. Paste the address of one YouTube or Vimeo video.');
            expect(within(videoForm()).getByRole('button', { name: 'Create' })).toBeDisabled();
        }
        // The rest of the form waits for an address that is recognised.
        expect(screen.queryByLabelText('Title')).not.toBeInTheDocument();
    });

    it('sends the address as typed and lets the server decide the rest', async () => {
        const { created, onCreated } = setUp({ orgId: 9, orgName: 'Team' });
        expect(screen.getByText('Sharing in Team')).toBeVisible();
        type(' https://youtu.be/dQw4w9WgXcQ?t=42 ');
        fireEvent.change(screen.getByLabelText('Title'), { target: { value: ' Town hall ' } });
        fireEvent.change(screen.getByLabelText('Button label'), { target: { value: ' Join us ' } });
        fireEvent.change(screen.getByLabelText('Button link'), { target: { value: 'https://example.org/join' } });
        fireEvent.change(screen.getByLabelText('Custom alias'), { target: { value: 'townhall' } });
        fireEvent.change(screen.getByLabelText('Expires (optional)'), { target: { value: '2031-05-17' } });
        create();

        await waitFor(() => expect(created()).toHaveLength(1));
        const body = created()[0].body!;
        expect(body).toEqual({
            original_url: 'https://youtu.be/dQw4w9WgXcQ?t=42',
            media: { cta_label: 'Join us', cta_url: 'https://example.org/join' },
            custom_alias: 'townhall',
            title: 'Town hall',
            expires_at: new Date('2031-05-17T23:59:00').toISOString(),
            org_id: 9,
        });
        // No player address, provider or video id is ever sent: the server reads them.
        expect(JSON.stringify(body)).not.toMatch(/embed|nocookie|media_id|provider/);

        await waitFor(() => expect(screen.getByText(/Video shared:/)).toHaveTextContent('/town-hall'));
        expect(onCreated).toHaveBeenCalledTimes(1);
        expect(address()).toHaveValue('');
        expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    });

    it('sends an empty media part when there is no button, and no title when none was typed', async () => {
        const { created } = setUp();
        type('https://vimeo.com/76979871');
        create();

        await waitFor(() => expect(created()).toHaveLength(1));
        expect(created()[0].body).toEqual({ original_url: 'https://vimeo.com/76979871', media: {} });
    });

    it('shows the server\'s refusal in its own words and keeps what was typed', async () => {
        const refusal = 'This video cannot be shown on a media page: its owner has turned embedding off, or it is private.';
        const { onCreated } = setUp({ links: { status: 400, body: { error: refusal } } });
        type('https://youtu.be/dQw4w9WgXcQ');
        create();

        expect(await screen.findByRole('alert')).toHaveTextContent(refusal);
        expect(screen.queryByText(/Video shared:/)).not.toBeInTheDocument();
        expect(onCreated).not.toHaveBeenCalled();
        expect(address()).toHaveValue('https://youtu.be/dQw4w9WgXcQ');
    });

    it('says so when the video could not be checked', async () => {
        setUp({ links: { status: 201, body: { id: 5, code: 'town-hall', media: { provider: 'youtube', embed_check: 'not_checked' } } } });
        type('https://youtu.be/dQw4w9WgXcQ');
        create();

        expect(await screen.findByText(/Video shared:/)).toBeVisible();
        expect(screen.getByRole('alert')).toHaveTextContent('We could not check that this video can be embedded. Open the link once to make sure it plays.');
    });

    it.each([
        ['Join us', '', /both a label and a link/],
        ['', 'https://example.org/join', /both a label and a link/],
        ['Go', 'javascript:alert(1)', /https:\/\//],
        ['Go', 'http://example.org/', /https:\/\//],
    ])('refuses the button %j / %j before anything is sent', async (label, link, message) => {
        const { created } = setUp();
        type('https://youtu.be/dQw4w9WgXcQ');
        fireEvent.change(screen.getByLabelText('Button label'), { target: { value: label } });
        fireEvent.change(screen.getByLabelText('Button link'), { target: { value: link } });
        create();

        expect(await screen.findByRole('alert')).toHaveTextContent(message);
        await settled();
        expect(created()).toHaveLength(0);
    });

    it('sends nothing for an address that is not recognised', async () => {
        const { created } = setUp();
        type('https://example.org/video');
        create();

        expect(await screen.findByRole('alert')).toHaveTextContent('Paste the address of one YouTube or Vimeo video.');
        await settled();
        expect(created()).toHaveLength(0);
    });
});

describe('the Media tab: a video in Google Drive', () => {
    beforeEach(() => {
        forgetDriveToken();
        vi.mocked(global.fetch).mockReset();
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
    });

    it('asks Google for nothing until the button is clicked', async () => {
        const { google } = setUp();
        type('https://youtu.be/dQw4w9WgXcQ');
        await settled();

        expect(driveButton()).toBeEnabled();
        expect(google.tokenRequests).toHaveLength(0);
        expect(google.driveCalls).toHaveLength(0);
        expect(google.pickerCalls).toHaveLength(0);
        expect(google.scriptsLoaded).toHaveLength(0);
    });

    it('is disabled where the picker is not set up', () => {
        setUp({ appSettings: { ...settings, drive: undefined } });
        expect(driveButton()).toBeDisabled();
        expect(driveButton()).toHaveTextContent('Not set up on this server');
    });

    it('opens the picker on Google\'s videos view', async () => {
        const { google } = setUp({ file: { permissions: PUBLIC } });
        await pickVideo();

        const views = google.pickerCalls.filter(([name]) => name === 'addView').map(([, calls]) => (calls as unknown[][])[0]);
        expect(views).toEqual([['DocsView', 'videos'], ['DocsView', 'videos']]);
    });

    it('a restricted video, box left unticked: the same warning and offer as a Drive file, and nothing changes in Drive', async () => {
        const { created, permissionPosts, google } = setUp();
        const form = await pickVideo();

        expect(within(form).getByText('Town hall.mp4')).toBeVisible();
        expect(within(form).getByText('Restricted: only people it is shared with')).toBeVisible();
        expect(within(form).getByRole('alert')).toHaveTextContent('The video will not play for people outside.');
        const tick = within(form).getByRole('checkbox', { name: 'Change to: anyone with the link can view' });
        expect(tick).not.toBeChecked();
        expect(within(form).getByLabelText('Title')).toHaveValue('Town hall.mp4');

        fireEvent.change(within(form).getByLabelText('Button label'), { target: { value: 'Join us' } });
        fireEvent.change(within(form).getByLabelText('Button link'), { target: { value: 'https://example.org/join' } });
        fireEvent.submit(form);

        await waitFor(() => expect(created()).toHaveLength(1));
        expect(created()[0].body).toEqual({
            original_url: VIDEO.webViewLink,
            drive_file: { file_id: 'video-1', name: 'Town hall.mp4', mime_type: 'video/mp4' },
            media: { provider: 'drive', cta_label: 'Join us', cta_url: 'https://example.org/join' },
            title: 'Town hall.mp4',
        });
        // No change of sharing was sent: the box was never ticked.
        expect(permissionPosts()).toHaveLength(0);
        expect(google.driveCalls.every(call => call.method === 'GET')).toBe(true);
        expect(await screen.findByText(/Video shared:/)).toHaveTextContent('/town-hall');
        expect(screen.getByText(/The video's sharing was not changed. It will not play for people it is not shared with/)).toBeVisible();
    });

    it('box ticked: the sharing is changed once, before the link is made', async () => {
        const { created, permissionPosts, order } = setUp();
        const form = await pickVideo();
        fireEvent.click(within(form).getByRole('checkbox', { name: 'Change to: anyone with the link can view' }));
        expect(within(form).getByRole('button', { name: 'Change sharing and create' })).toBeVisible();
        fireEvent.submit(form);

        await waitFor(() => expect(created()).toHaveLength(1));
        expect(permissionPosts()).toHaveLength(1);
        expect(permissionPosts()[0].body).toEqual({ type: 'anyone', role: 'reader' });
        expect(order.indexOf('drive POST')).toBeLessThan(order.indexOf('api POST /links'));
        expect((created()[0].body!.media as Record<string, unknown>).provider).toBe('drive');
    });

    it('a button our server would refuse is caught before the sharing is changed', async () => {
        const { created, permissionPosts } = setUp();
        const form = await pickVideo();
        fireEvent.click(within(form).getByRole('checkbox', { name: 'Change to: anyone with the link can view' }));
        fireEvent.change(within(form).getByLabelText('Button label'), { target: { value: 'Go' } });
        fireEvent.change(within(form).getByLabelText('Button link'), { target: { value: 'javascript:alert(1)' } });
        fireEvent.submit(form);

        expect(await screen.findByText(/must be an https:\/\/ address/)).toBeVisible();
        await settled();
        expect(permissionPosts()).toHaveLength(0);
        expect(created()).toHaveLength(0);
    });

    it('a file that is not a video is not offered at all', async () => {
        const { created, permissionPosts } = setUp({
            file: { name: 'Board plan.pdf', mimeType: 'application/pdf', webViewLink: 'https://drive.google.com/file/d/video-1/view' },
        });
        fireEvent.click(driveButton());

        expect(await screen.findByRole('alert')).toHaveTextContent('"Board plan.pdf" was not added. Only a video can be shared on a media page.');
        expect(screen.queryByRole('form', { name: 'Share a Google Drive video' })).not.toBeInTheDocument();
        expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
        await settled();
        expect(permissionPosts()).toHaveLength(0);
        expect(created()).toHaveLength(0);
    });
});

describe('a media link in the list and in the edit dialog', () => {
    const link: LinkData = {
        id: 7,
        code: 'town-hall',
        original_url: 'https://www.youtube.com/watch?v=dQw4w9WgXcQ',
        short_url: 'http://localhost:3000/town-hall',
        title: 'Town hall',
        click_count: 3,
        created_at: '2026-01-01T00:00:00Z',
        expires_at: null,
        has_password: false,
        notes: null,
        is_active: true,
        is_pinned: false,
        tags: [],
        media: { provider: 'youtube', cta_label: 'Join us', cta_url: 'https://example.org/join' },
    };

    function row(shown: LinkData) {
        return render(
            <LinkRow
                link={shown}
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

    beforeEach(() => {
        vi.mocked(global.fetch).mockReset();
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
    });

    it.each([
        ['youtube', 'YouTube video'],
        ['vimeo', 'Vimeo video'],
        ['drive', 'Google Drive video'],
    ])('shows a play icon and the provider for a %s link', (provider, text) => {
        row({ ...link, media: { provider } });

        const badge = document.querySelector('[data-media-link="7"]')!;
        expect(badge).toHaveTextContent(text);
        expect(badge.querySelector('svg')).toBeInTheDocument();
    });

    it('shows no such badge on an ordinary link', () => {
        row({ ...link, media: null });
        expect(document.querySelector('[data-media-link]')).not.toBeInTheDocument();
    });

    it('edits the button and leaves the destination alone', async () => {
        const onSave = vi.fn().mockResolvedValue(undefined);
        render(<EditModal link={link} onClose={vi.fn()} onSave={onSave} routingEnabled />);

        // The address is shown, not edited, and there are no routing rules.
        // (The dialog fades in, so "visible" is asked of the markup, not of opacity.)
        expect(screen.getByText('https://www.youtube.com/watch?v=dQw4w9WgXcQ').closest('[hidden]')).toBeNull();
        expect(screen.getByLabelText('Destination URL').closest('[hidden]')).not.toBeNull();
        expect(screen.queryByRole('button', { name: /smart routing/i })).not.toBeInTheDocument();
        expect(vi.mocked(global.fetch).mock.calls.some(([url]) => String(url).includes('/rules'))).toBe(false);
        expect(screen.getByLabelText('Button label')).toHaveValue('Join us');

        // One half alone is refused here.
        fireEvent.change(screen.getByLabelText('Button link'), { target: { value: '' } });
        fireEvent.click(screen.getByRole('button', { name: /^save/i }));
        expect(await screen.findByText(/both a label and a link/)).toBeInTheDocument();
        expect(onSave).not.toHaveBeenCalled();

        fireEvent.change(screen.getByLabelText('Button label'), { target: { value: 'Donate' } });
        fireEvent.change(screen.getByLabelText('Button link'), { target: { value: 'https://example.org/give' } });
        fireEvent.click(screen.getByRole('button', { name: /^save/i }));
        await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
        const [, payload] = onSave.mock.calls[0];
        expect(payload.media_cta).toEqual({ label: 'Donate', url: 'https://example.org/give' });
        expect(payload.original_url).toBeUndefined();
    });

    it('sends no button when it was not changed', async () => {
        const onSave = vi.fn().mockResolvedValue(undefined);
        render(<EditModal link={link} onClose={vi.fn()} onSave={onSave} />);
        fireEvent.click(screen.getByRole('button', { name: /^save/i }));

        await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
        expect(onSave.mock.calls[0][1].media_cta).toBeUndefined();
    });
});
