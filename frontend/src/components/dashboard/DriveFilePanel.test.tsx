import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, mockToken, render, screen, waitFor } from '../../test/test-utils';
import { DRIVE_TOKEN, driveFile, driveJson, fakeGoogle, type DriveCall, type FakeGoogleOptions } from '../../test/fakeGoogle';
import { forgetDriveToken } from '../../utils/googleDrive';
import CreateFilesTab from './CreateFilesTab';
import DriveFilePanel from './DriveFilePanel';
import LinkRow from './LinkRow';
import type { AppSettings, LinkData } from './types';

// "Google Drive Files": the real panel and the real Drive module, with Google
// itself replaced by `fakeGoogle`. `global.fetch` is our own API; the Drive
// API is `google.env.fetch`. `order` records both, so a test can say which
// request went first.

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

const PUBLIC = [{ type: 'user', role: 'owner' }, { type: 'anyone', role: 'reader' }];

interface ApiCall { url: string; method: string; body: unknown; headers: Headers }
interface ApiAnswer { status: number; body: unknown }
/** Our API cannot be reached at all. */
const NETWORK_DOWN = 'network down';

function setUp(options: {
    file?: Record<string, unknown>;
    /** Files by id, when a test picks more than one. */
    files?: Record<string, Record<string, unknown>>;
    /** Google's answer to the permission change. Default: it is made. */
    permission?: { status: number; body: unknown };
    /** Our answer to `POST /links`; a list answers the first, second, ... request. */
    links?: ApiAnswer | typeof NETWORK_DOWN | (ApiAnswer | typeof NETWORK_DOWN)[];
    /** Our answer to the alias check. Default: the alias is free. */
    aliasCheck?: ApiAnswer | typeof NETWORK_DOWN;
    google?: FakeGoogleOptions;
    appSettings?: AppSettings;
    orgId?: number | null;
} = {}) {
    const order: string[] = [];
    const apiCalls: ApiCall[] = [];
    const google = fakeGoogle({
        drive: (call: DriveCall) => call.method === 'POST'
            ? options.permission ?? driveJson({ id: 'anyoneWithLink' })
            : driveJson(driveFile(options.files?.[new URL(call.url).pathname.split('/')[4]] ?? options.file)),
        onDriveCall: (call) => order.push(`drive ${call.method} ${new URL(call.url).pathname}`),
        ...options.google,
    });
    const linkAnswers = Array.isArray(options.links) ? [...options.links] : null;
    const respond = (answer: ApiAnswer | typeof NETWORK_DOWN) => {
        if (answer === NETWORK_DOWN) throw new TypeError('Failed to fetch');
        return { ok: answer.status < 300, status: answer.status, json: () => Promise.resolve(answer.body) } as Response;
    };
    vi.mocked(global.fetch).mockImplementation(async (input, init) => {
        const url = String(input);
        const method = init?.method ?? 'GET';
        apiCalls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : undefined, headers: new Headers(init?.headers) });
        if (method === 'POST' && url.endsWith('/links')) {
            order.push('api POST /links');
            const made = { status: 201, body: { id: 5, code: 'board-plan' } };
            return respond(linkAnswers ? linkAnswers.shift() ?? made : (options.links as ApiAnswer | typeof NETWORK_DOWN | undefined) ?? made);
        }
        // The alias check.
        order.push('api GET alias check');
        return respond(options.aliasCheck ?? { status: 200, body: { available: true, code: '', message: '' } });
    });
    const onCreated = vi.fn();
    render(
        <DriveFilePanel
            appSettings={options.appSettings ?? settings}
            onCreated={onCreated}
            orgId={options.orgId}
            driveEnv={google.env}
        />,
    );
    const created = () => apiCalls.filter(call => call.method === 'POST' && call.url.endsWith('/links'));
    const permissionPosts = () => google.driveCalls.filter(call => call.method === 'POST');
    return { google, order, apiCalls, onCreated, created, permissionPosts };
}

async function pick() {
    fireEvent.click(screen.getByRole('button', { name: /Google Drive Files/ }));
    return screen.findByRole('form', { name: 'Share a Google Drive file' });
}

const tick = () => screen.getByRole('checkbox', { name: 'Change to: anyone with the link can view' });
const submit = () => fireEvent.submit(screen.getByRole('form', { name: 'Share a Google Drive file' }));
const driveButton = () => screen.getByRole('button', { name: /Google Drive Files/ });
/** Long enough for anything that was going to happen to have happened. */
const settled = () => new Promise(resolve => setTimeout(resolve, 30));

const SHARING_WAS_CHANGED = 'The file\'s sharing in Google Drive was changed to "anyone with the link can view", but no link was created.';
const AFTER_CHANGE_ADVICE = 'You can try again here (the sharing will not be changed a second time) or change the sharing back in Google Drive.';
const FILE_A = { id: 'file-a', name: 'First file', webViewLink: 'https://docs.google.com/document/d/file-a/edit' };
const FILE_B = { id: 'file-b', name: 'Second file', webViewLink: 'https://docs.google.com/document/d/file-b/edit' };

describe('Google Drive Files', () => {
    beforeEach(() => {
        forgetDriveToken();
        vi.mocked(global.fetch).mockReset();
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
        vi.mocked(localStorage.setItem).mockClear();
    });

    it.each([
        ['no drive settings', { ...settings, drive: undefined }],
        ['no Google sign-in', { ...settings, google_client_id: undefined }],
    ])('is disabled and says it is not set up with %s', (_name, appSettings) => {
        const { google } = setUp({ appSettings });

        const button = screen.getByRole('button', { name: /Google Drive Files/ });
        expect(button).toBeDisabled();
        expect(button).toHaveTextContent('Not set up on this server');
        fireEvent.click(button);
        expect(google.tokenRequests).toHaveLength(0);
        expect(global.fetch).not.toHaveBeenCalled();
    });

    it('asks Google for nothing until the button is clicked', () => {
        const { google } = setUp();

        expect(screen.getByRole('button', { name: /Google Drive Files/ })).toBeEnabled();
        expect(google.tokenRequests).toHaveLength(0);
        expect(google.driveCalls).toHaveLength(0);
        expect(google.pickerCalls).toHaveLength(0);
        expect(global.fetch).not.toHaveBeenCalled();
    });

    it('asks for the drive.file scope only, as the signed-in person, and opens the picker with the server\'s key', async () => {
        const { google } = setUp({ file: { permissions: PUBLIC } });
        await pick();

        expect(google.tokenRequests).toHaveLength(1);
        expect(google.tokenRequests[0].scope).toBe('https://www.googleapis.com/auth/drive.file');
        expect(google.tokenRequests[0].client_id).toBe('client-1.apps.googleusercontent.com');
        // The email in our own sign-in token.
        expect(google.tokenRequests[0].login_hint).toBe('test@example.com');
        expect(google.pickerCalls).toContainEqual(['setDeveloperKey', 'browser-key']);
        expect(google.pickerCalls).toContainEqual(['setAppId', '1234567890']);
    });

    it('a file anyone can already open: shows that, offers no change, and never asks Google to change it', async () => {
        const { google, created, permissionPosts, onCreated } = setUp({ file: { permissions: PUBLIC } });
        await pick();

        expect(screen.getByText('Board plan')).toBeVisible();
        expect(screen.getByText('Anyone with the link can view')).toBeVisible();
        expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
        expect(screen.queryByText(/will not be able to open/)).not.toBeInTheDocument();
        // The title starts as the file's name.
        expect(screen.getByLabelText('Title')).toHaveValue('Board plan');

        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared: localhost:3000/board-plan'));

        expect(permissionPosts()).toHaveLength(0);
        expect(google.driveCalls.every(call => call.method === 'GET')).toBe(true);
        expect(created()).toHaveLength(1);
        expect(created()[0].body).toEqual({
            original_url: 'https://docs.google.com/document/d/file-1/edit?usp=drivesdk',
            drive_file: { file_id: 'file-1', name: 'Board plan', mime_type: 'application/vnd.google-apps.document' },
            title: 'Board plan',
        });
        expect(onCreated).toHaveBeenCalledTimes(1);
        // No warning after the link: everyone can open it.
        expect(screen.queryByText(/request access/)).not.toBeInTheDocument();
    });

    it('a restricted file, box left unticked: warns, creates the link, changes nothing in Drive, and warns again', async () => {
        const { created, permissionPosts } = setUp();
        await pick();

        expect(screen.getByText('Restricted: only people it is shared with')).toBeVisible();
        expect(screen.getByText('People outside will not be able to open this file.')).toBeVisible();
        expect(tick()).not.toBeChecked();
        expect(tick()).toBeEnabled();

        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared: localhost:3000/board-plan'));

        expect(permissionPosts()).toHaveLength(0);
        expect(created()).toHaveLength(1);
        expect(screen.getByText(/The file's sharing was not changed\..*request access/)).toBeVisible();
    });

    it('a restricted file, box ticked: changes the sharing first, then creates the link', async () => {
        const { order, created, permissionPosts } = setUp();
        await pick();

        fireEvent.click(tick());
        // Ticking alone sends nothing.
        expect(permissionPosts()).toHaveLength(0);
        expect(screen.getByRole('button', { name: 'Change sharing and create' })).toBeVisible();

        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));

        expect(permissionPosts()).toHaveLength(1);
        expect(permissionPosts()[0].body).toEqual({ type: 'anyone', role: 'reader' });
        expect(permissionPosts()[0].url).toBe('https://www.googleapis.com/drive/v3/files/file-1/permissions?supportsAllDrives=true');
        expect(created()).toHaveLength(1);
        const changed = order.indexOf('drive POST /drive/v3/files/file-1/permissions');
        expect(changed).toBeGreaterThan(-1);
        expect(changed).toBeLessThan(order.indexOf('api POST /links'));
        // Everyone can open it now: no warning.
        expect(screen.queryByText(/request access/)).not.toBeInTheDocument();
    });

    it('Google refuses the change: shows Google\'s words and creates no link until the box is unticked', async () => {
        const { created, permissionPosts, onCreated } = setUp({
            permission: driveJson({ error: { message: 'Sharing outside the organization is not allowed.' } }, 403),
        });
        await pick();
        fireEvent.click(tick());

        submit();
        await waitFor(() => expect(screen.getByText(/Sharing outside the organization is not allowed\./)).toBeVisible());

        expect(screen.getByText(/No link was created\./)).toBeVisible();
        expect(created()).toHaveLength(0);
        expect(onCreated).not.toHaveBeenCalled();
        // The file and what was typed are still there, and the box is still ticked.
        expect(tick()).toBeChecked();

        fireEvent.click(tick());
        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));
        expect(permissionPosts()).toHaveLength(1);
        expect(created()).toHaveLength(1);
    });

    it('a file this person cannot share: the box is disabled and says why', async () => {
        const { permissionPosts, created } = setUp({ file: { capabilities: { canShare: false } } });
        await pick();

        expect(tick()).toBeDisabled();
        expect(screen.getByText(/You cannot change this file's sharing\./)).toBeVisible();

        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));
        expect(permissionPosts()).toHaveLength(0);
        expect(created()).toHaveLength(1);
    });

    it('a file shared with a domain names the domain and is treated as locked to outsiders', async () => {
        setUp({ file: { permissions: [{ type: 'domain', role: 'reader', domain: 'example.org' }] } });
        await pick();

        expect(screen.getByText('Only people at example.org with the link')).toBeVisible();
        expect(tick()).not.toBeChecked();
    });

    it('sends the alias, the expiry and the team with the link', async () => {
        const { created } = setUp({ file: { permissions: PUBLIC }, orgId: 12 });
        await pick();

        fireEvent.change(screen.getByLabelText('Custom alias'), { target: { value: 'plan-2027' } });
        fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'The plan' } });
        fireEvent.change(screen.getByLabelText('Expires (optional)'), { target: { value: '2099-01-02' } });
        submit();
        await waitFor(() => expect(created()).toHaveLength(1));

        expect(created()[0].body).toMatchObject({
            custom_alias: 'plan-2027',
            title: 'The plan',
            org_id: 12,
            expires_at: new Date('2099-01-02T23:59:00').toISOString(),
        });
    });

    it('keeps the file and says why when our server refuses the link', async () => {
        const { onCreated } = setUp({ file: { permissions: PUBLIC }, links: { status: 409, body: { error: 'Alias already taken' } } });
        await pick();

        submit();
        await waitFor(() => expect(screen.getByText('Alias already taken')).toBeVisible());
        expect(screen.getByText('Board plan')).toBeVisible();
        expect(onCreated).not.toHaveBeenCalled();
    });

    it('closing the picker without picking says nothing', async () => {
        const { google } = setUp({ google: { pick: null } });

        fireEvent.click(screen.getByRole('button', { name: /Google Drive Files/ }));
        await waitFor(() => expect(google.pickerCalls).toContainEqual(['setVisible', true]));
        await waitFor(() => expect(screen.getByRole('button', { name: /Google Drive Files/ })).toBeEnabled());

        expect(screen.queryByRole('alert')).not.toBeInTheDocument();
        expect(screen.queryByRole('form')).not.toBeInTheDocument();
        expect(google.driveCalls).toHaveLength(0);
    });

    it.each([
        ['the Google window is closed', { token: { errorType: 'popup_closed' } }, /Google window was closed/],
        ['Drive access is declined', { token: { error: 'access_denied' } }, /Google Drive access was not given/],
        ['Drive refuses the file', { drive: () => driveJson({ error: { message: 'File not found: file-1.' } }, 404) }, /Google Drive said: File not found: file-1\./],
    ])('says so in plain words when %s', async (_name, googleOptions, message) => {
        setUp({ google: googleOptions as FakeGoogleOptions });

        fireEvent.click(screen.getByRole('button', { name: /Google Drive Files/ }));

        await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent(message));
        expect(screen.queryByRole('form')).not.toBeInTheDocument();
    });

    it('never sends the Google token to our API and never stores it', async () => {
        const setSession = vi.spyOn(Storage.prototype, 'setItem');
        const { apiCalls, google, created } = setUp();
        await pick();
        fireEvent.change(screen.getByLabelText('Custom alias'), { target: { value: 'plan-2027' } });
        fireEvent.click(tick());
        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));

        // Google got it; our API did not, in the address, a header or the body.
        expect(google.driveCalls.every(call => call.authorization === `Bearer ${DRIVE_TOKEN}`)).toBe(true);
        expect(apiCalls.length).toBeGreaterThan(0);
        for (const call of apiCalls) {
            expect(call.url).not.toContain(DRIVE_TOKEN);
            expect(JSON.stringify(call.body ?? null)).not.toContain(DRIVE_TOKEN);
            expect([...call.headers.values()].join(' ')).not.toContain(DRIVE_TOKEN);
        }
        expect(created()[0].headers.get('Authorization')).toBe(`Bearer ${mockToken}`);
        expect(localStorage.setItem).not.toHaveBeenCalled();
        expect(setSession).not.toHaveBeenCalled();
        setSession.mockRestore();
    });
});

// What happens around a sharing change. The rule: a file's sharing is never
// changed and then left without a link and without the person knowing.
describe('Google Drive Files: nothing our server would refuse changes a file\'s sharing', () => {
    beforeEach(() => {
        forgetDriveToken();
        vi.mocked(global.fetch).mockReset();
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
    });

    it.each([
        ['Apps Script', 'https://script.google.com/d/file-1/edit'],
        ['Colab', 'https://colab.research.google.com/drive/file-1'],
        ['plain http', 'http://docs.google.com/document/d/file-1/edit'],
    ])('a file Google opens somewhere else (%s) is refused when it is picked: no tick, no Create', async (_name, webViewLink) => {
        const { permissionPosts, created } = setUp({ file: { webViewLink } });

        fireEvent.click(driveButton());

        await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent(
            '"Board plan" was not added. This kind of Google file cannot be shared from here yet.',
        ));
        expect(screen.queryByRole('form')).not.toBeInTheDocument();
        expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
        expect(screen.queryByRole('button', { name: /Create/ })).not.toBeInTheDocument();
        expect(driveButton()).toBeEnabled();
        expect(permissionPosts()).toHaveLength(0);
        expect(created()).toHaveLength(0);
    });

    it('an unsupported file replaces the one picked before it, so nothing of the first is left to create', async () => {
        setUp({
            google: { picks: ['file-a', 'file-b'] },
            files: { 'file-a': FILE_A, 'file-b': { ...FILE_B, webViewLink: 'https://sites.google.com/view/x' } },
        });
        await pick();
        fireEvent.click(tick());

        fireEvent.click(driveButton());

        await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('"Second file" was not added.'));
        expect(screen.queryByRole('form')).not.toBeInTheDocument();
    });

    it('a name and a type our server would refuse are cleaned, so the link is made after the change', async () => {
        const { created, permissionPosts } = setUp({
            file: { name: `Plan\t2027\n${'n'.repeat(600)}`, mimeType: `application/\u0007pdf${'x'.repeat(300)}` },
        });
        await pick();
        fireEvent.click(tick());
        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));

        expect(permissionPosts()).toHaveLength(1);
        const body = created()[0].body as { title: string; drive_file: { name: string; mime_type: string } };
        expect(body.drive_file.name).toBe(`Plan2027${'n'.repeat(492)}`);
        expect(body.drive_file.name).toHaveLength(500);
        expect(body.title).toBe(body.drive_file.name);
        expect(body.drive_file.mime_type).toBe(`application/pdf${'x'.repeat(240)}`);
    });

    it('a title that is too long is refused before the sharing is changed', async () => {
        const { created, permissionPosts } = setUp();
        await pick();
        fireEvent.click(tick());
        fireEvent.change(screen.getByLabelText('Title'), { target: { value: 't'.repeat(501) } });

        submit();

        await waitFor(() => expect(screen.getByText('Title must be at most 500 characters')).toBeVisible());
        await settled();
        expect(permissionPosts()).toHaveLength(0);
        expect(created()).toHaveLength(0);
    });

    it('an alias that is taken is found out before the sharing is changed', async () => {
        const { created, permissionPosts, order } = setUp({
            aliasCheck: { status: 200, body: { available: false, code: 'plan-2027', message: 'This alias is already taken' } },
        });
        await pick();
        fireEvent.change(screen.getByLabelText('Custom alias'), { target: { value: 'plan-2027' } });
        fireEvent.click(tick());

        submit();

        await waitFor(() => expect(screen.getByText(
            'This alias is already taken. The file\'s sharing was not changed and no link was created.',
        )).toBeVisible());
        await settled();
        expect(order).toContain('api GET alias check');
        expect(permissionPosts()).toHaveLength(0);
        expect(created()).toHaveLength(0);
        // Nothing was lost: the file, the alias and the tick are still there.
        expect(tick()).toBeChecked();
        expect(screen.getByLabelText('Custom alias')).toHaveValue('plan-2027');
    });

    it('an alias that cannot be checked changes nothing either', async () => {
        const { created, permissionPosts } = setUp({ aliasCheck: NETWORK_DOWN });
        await pick();
        fireEvent.change(screen.getByLabelText('Custom alias'), { target: { value: 'plan-2027' } });
        fireEvent.click(tick());

        submit();

        await waitFor(() => expect(screen.getByText(
            /The alias could not be checked, so the file's sharing was not changed and no link was created\./,
        )).toBeVisible());
        await settled();
        expect(permissionPosts()).toHaveLength(0);
        expect(created()).toHaveLength(0);
    });

    it('checks the alias before the change, and the change before the link', async () => {
        const { order } = setUp();
        await pick();
        fireEvent.change(screen.getByLabelText('Custom alias'), { target: { value: 'plan-2027' } });
        fireEvent.click(tick());
        order.length = 0;

        submit();
        await waitFor(() => expect(screen.getByText(/File shared:/)).toBeVisible());

        const change = order.indexOf('drive POST /drive/v3/files/file-1/permissions');
        expect(order.slice(0, change)).toContain('api GET alias check');
        expect(change).toBeLessThan(order.indexOf('api POST /links'));
    });
});

describe('Google Drive Files: a link that fails after the sharing was changed says so', () => {
    beforeEach(() => {
        forgetDriveToken();
        vi.mocked(global.fetch).mockReset();
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
    });

    it('our server refuses the link: the message says the sharing changed, and a retry does not change it again', async () => {
        const { created, permissionPosts, onCreated } = setUp({
            links: [{ status: 403, body: { error: 'You have reached your link limit' } }],
        });
        await pick();
        fireEvent.click(tick());

        submit();

        await waitFor(() => expect(screen.getByText(
            `${SHARING_WAS_CHANGED} Reason: You have reached your link limit. ${AFTER_CHANGE_ADVICE}`,
        )).toBeVisible());
        expect(permissionPosts()).toHaveLength(1);
        expect(created()).toHaveLength(1);
        expect(onCreated).not.toHaveBeenCalled();
        // The panel shows the file as it now is, and keeps saying what was done.
        expect(screen.getByText('Anyone with the link can view')).toBeVisible();
        expect(screen.getByRole('note')).toHaveTextContent(
            'You changed this file\'s sharing to "anyone with the link can view" a moment ago. No link has been created for it yet.',
        );
        expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();

        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));

        // One change in Drive, however many times the link was tried.
        expect(permissionPosts()).toHaveLength(1);
        expect(created()).toHaveLength(2);
        expect(screen.queryByRole('note')).not.toBeInTheDocument();
        expect(screen.queryByText(/was changed/)).not.toBeInTheDocument();
    });

    it('a second failure still says the sharing was changed', async () => {
        const { permissionPosts } = setUp({
            links: [{ status: 403, body: { error: 'First refusal' } }, { status: 409, body: { error: 'Alias already taken' } }],
        });
        await pick();
        fireEvent.click(tick());
        submit();
        await waitFor(() => expect(screen.getByText(/Reason: First refusal\./)).toBeVisible());

        submit();

        await waitFor(() => expect(screen.getByText(
            `${SHARING_WAS_CHANGED} Reason: Alias already taken. ${AFTER_CHANGE_ADVICE}`,
        )).toBeVisible());
        expect(permissionPosts()).toHaveLength(1);
    });

    it('our server cannot be reached: the message says the sharing changed', async () => {
        const { permissionPosts, created } = setUp({ links: NETWORK_DOWN });
        await pick();
        fireEvent.click(tick());

        submit();

        await waitFor(() => expect(screen.getByText(
            `${SHARING_WAS_CHANGED} Reason: The link could not be created. Check your connection and try again. ${AFTER_CHANGE_ADVICE}`,
        )).toBeVisible());
        expect(permissionPosts()).toHaveLength(1);
        expect(created()).toHaveLength(1);
    });

    it('the session ended: the message says so and says the sharing changed', async () => {
        setUp({ links: { status: 401, body: { error: 'Unauthorized' } } });
        await pick();
        fireEvent.click(tick());

        submit();

        await waitFor(() => expect(screen.getByText(
            `${SHARING_WAS_CHANGED} Reason: Session expired. Please log in again. ${AFTER_CHANGE_ADVICE}`,
        )).toBeVisible());
    });

    it('a failed link says nothing about sharing when the sharing was not changed', async () => {
        setUp({ links: { status: 403, body: { error: 'You have reached your link limit' } } });
        await pick();

        submit();

        await waitFor(() => expect(screen.getByText('You have reached your link limit')).toBeVisible());
        expect(screen.queryByText(/was changed/)).not.toBeInTheDocument();
        expect(screen.queryByRole('note')).not.toBeInTheDocument();
    });
});

describe('Google Drive Files: the tick, the click and the picker', () => {
    beforeEach(() => {
        forgetDriveToken();
        vi.mocked(global.fetch).mockReset();
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
    });

    it('a tick made for one file does not carry over to the next file picked', async () => {
        const { permissionPosts, created } = setUp({
            google: { picks: ['file-a', 'file-b'] },
            files: { 'file-a': FILE_A, 'file-b': FILE_B },
        });
        await pick();
        expect(screen.getByText('First file')).toBeVisible();
        fireEvent.click(tick());
        expect(tick()).toBeChecked();

        fireEvent.click(driveButton());
        await waitFor(() => expect(screen.getByText('Second file')).toBeVisible());

        expect(tick()).not.toBeChecked();
        expect(screen.getByRole('button', { name: 'Create link' })).toBeVisible();
        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));
        expect(permissionPosts()).toHaveLength(0);
        expect((created()[0].body as { drive_file: { file_id: string } }).drive_file.file_id).toBe('file-b');
    });

    it('nor to the same file removed and picked again', async () => {
        const { permissionPosts } = setUp();
        await pick();
        fireEvent.click(tick());

        fireEvent.click(screen.getByRole('button', { name: 'Remove Google Drive file' }));
        expect(screen.queryByRole('form')).not.toBeInTheDocument();
        await pick();

        expect(tick()).not.toBeChecked();
        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));
        expect(permissionPosts()).toHaveLength(0);
    });

    it('three submits in a row change the sharing once and make one link', async () => {
        const { permissionPosts, created } = setUp();
        await pick();
        fireEvent.click(tick());

        submit();
        submit();
        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));
        await settled();

        expect(permissionPosts()).toHaveLength(1);
        expect(created()).toHaveLength(1);
    });

    it('a picker that never answers gives the button back, and a second click opens a new one', async () => {
        const { google } = setUp({ google: { pickerSilent: true } });

        fireEvent.click(driveButton());
        await waitFor(() => expect(google.pickerCalls.filter(([name, shown]) => name === 'setVisible' && shown === true)).toHaveLength(1));

        await waitFor(() => expect(driveButton()).toBeEnabled());
        expect(driveButton()).not.toHaveTextContent('Opening');
        expect(screen.getByText('Google\'s file picker is open. If it did not appear, click Google Drive Files again.')).toBeVisible();
        expect(screen.queryByRole('alert')).not.toBeInTheDocument();

        fireEvent.click(driveButton());
        await waitFor(() => expect(google.pickerCalls.filter(([name, shown]) => name === 'setVisible' && shown === true)).toHaveLength(2));
        // The first picker was closed, and Google was not asked for another token.
        expect(google.pickerCalls).toContainEqual(['dispose']);
        expect(google.tokenRequests).toHaveLength(1);

        // An answer from the picker that was left behind is not used...
        google.answerPicker(0, 'file-1');
        await settled();
        expect(screen.queryByRole('form')).not.toBeInTheDocument();
        expect(screen.getByText(/file picker is open/)).toBeVisible();
        expect(driveButton()).toBeEnabled();
        // ...and the one that is open works as usual.
        google.answerPicker(1, 'file-1');
        expect(await screen.findByRole('form', { name: 'Share a Google Drive file' })).toBeVisible();
        expect(screen.queryByText(/file picker is open/)).not.toBeInTheDocument();
    });

    it('a picker that never answers does not stop the link for the file already picked', async () => {
        const { google, created } = setUp({ file: { permissions: PUBLIC } });
        await pick();
        google.options.pickerSilent = true;
        fireEvent.click(driveButton());
        await waitFor(() => expect(screen.getByText(/file picker is open/)).toBeVisible());

        submit();
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('File shared:'));
        expect(created()).toHaveLength(1);

        // Whatever that picker says afterwards is not used.
        google.answerPicker(1, 'file-1');
        await settled();
        expect(screen.queryByRole('form')).not.toBeInTheDocument();
        expect(screen.getByRole('status')).toHaveTextContent('File shared:');
    });

    it.each([
        ['cannot be built', { pickerThrows: true }],
        ['reports an error', { pickerErrors: true }],
    ])('a picker that %s says so and gives the button back', async (_name, googleOptions) => {
        setUp({ google: googleOptions });

        fireEvent.click(driveButton());

        await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent(
            'Google\'s file picker could not be opened. Click Google Drive Files to try again.',
        ));
        expect(driveButton()).toBeEnabled();
        expect(driveButton()).not.toHaveTextContent('Opening');
        expect(screen.queryByText(/file picker is open/)).not.toBeInTheDocument();
    });

    it.each([
        ['writer', 'Anyone with the link can edit'],
        ['commenter', 'Anyone with the link can comment'],
    ])('a file anyone can already open as %s is described as it is', async (role, words) => {
        setUp({ file: { permissions: [{ type: 'user', role: 'owner' }, { type: 'anyone', role }] } });
        await pick();

        expect(screen.getByText(words)).toBeVisible();
        expect(screen.queryByText('Anyone with the link can view')).not.toBeInTheDocument();
        expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    });
});

describe('the Files tab', () => {
    beforeEach(() => {
        vi.mocked(global.fetch).mockReset();
        vi.mocked(global.fetch).mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve({ bytes_used: 0, file_count: 0 }) } as Response);
        vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
    });

    it('offers Google Drive on a server with the picker and no upload storage', () => {
        render(<CreateFilesTab appSettings={settings} active onCreated={vi.fn()} />);

        expect(screen.getByRole('button', { name: /Google Drive Files/ })).toBeEnabled();
        expect(screen.getByRole('button', { name: /Microsoft OneDrive Files/ })).toBeDisabled();
        expect(screen.getByText('File sharing is not set up on this server.')).toBeVisible();
        expect(document.querySelector('script[src*="google"]')).toBeNull();
    });

    it('enables Google Drive beside the upload form when both are set up', () => {
        render(<CreateFilesTab appSettings={{ ...settings, files: { enabled: true, max_bytes: 1024 } }} active onCreated={vi.fn()} />);

        expect(screen.getByRole('button', { name: /Google Drive Files/ })).toBeEnabled();
        expect(screen.getByRole('button', { name: /Microsoft OneDrive Files/ })).toHaveTextContent('Coming soon');
        expect(screen.getByRole('heading', { name: 'Upload a File' })).toBeVisible();
    });
});

describe('a Google Drive link in the list', () => {
    const link: LinkData = {
        id: 9,
        code: 'board-plan',
        original_url: 'https://docs.google.com/document/d/file-1/edit?usp=drivesdk',
        short_url: 'http://localhost:3000/board-plan',
        title: 'Board plan',
        click_count: 0,
        created_at: '2026-01-01T00:00:00Z',
        expires_at: null,
        has_password: false,
        notes: null,
        is_active: true,
        is_pinned: false,
        tags: [],
        drive: { name: 'Board plan.gdoc', mime_type: 'application/vnd.google-apps.document' },
    };

    function row(shown: LinkData) {
        const noop = vi.fn();
        return render(
            <LinkRow
                link={shown}
                linkNumber={1}
                copied={false}
                sourceCopied={false}
                cloning={false}
                selected={false}
                onToggleSelect={noop}
                onCopy={noop}
                onCopySource={noop}
                onPreview={noop}
                onPin={noop}
                onClone={noop}
                onShare={noop}
                onQr={noop}
                onEdit={noop}
                onDelete={noop}
            />,
        );
    }

    it('shows the Drive mark and the file name, linked to the file in Drive, with the usual actions', () => {
        row(link);

        expect(screen.getByRole('img', { name: 'Google Drive file' })).toBeInTheDocument();
        const destination = screen.getByRole('link', { name: /Board plan\.gdoc/ });
        expect(destination).toHaveAttribute('href', link.original_url);
        expect(screen.queryByText(/docs\.google\.com/)).not.toBeInTheDocument();
        // An ordinary URL link otherwise.
        expect(screen.getByRole('button', { name: 'Copy source URL for board-plan' })).toBeInTheDocument();
        expect(screen.getByRole('button', { name: /Clone/ })).toBeInTheDocument();
    });

    it('shows the address as before for a link without a Drive file', () => {
        row({ ...link, drive: undefined });

        expect(screen.queryByRole('img', { name: 'Google Drive file' })).not.toBeInTheDocument();
        expect(screen.getByRole('link', { name: /docs\.google\.com\/document/ })).toBeInTheDocument();
    });
});
