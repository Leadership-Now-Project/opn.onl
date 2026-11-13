import { beforeEach, describe, expect, it, vi } from 'vitest';
import { DRIVE_TOKEN, driveFile, driveJson, fakeGoogle } from '../test/fakeGoogle';
import {
    DRIVE_FILE_SCOPE,
    describeSharing,
    driveAccessToken,
    DriveError,
    emailFromToken,
    forgetDriveToken,
    pickDriveFile,
    readDriveFile,
    shareWithAnyone,
    sharingOf,
} from './googleDrive';

const request = { clientId: 'client-1.apps.googleusercontent.com', loginHint: 'pat@example.test' };

describe('driveAccessToken', () => {
    beforeEach(() => {
        forgetDriveToken();
        vi.mocked(localStorage.setItem).mockClear();
    });

    it('asks Google for exactly the drive.file scope, without a consent prompt, for the signed-in person', async () => {
        const google = fakeGoogle();
        const setSession = vi.spyOn(Storage.prototype, 'setItem');

        await expect(driveAccessToken(request, google.env)).resolves.toBe(DRIVE_TOKEN);

        expect(google.tokenRequests).toHaveLength(1);
        const asked = google.tokenRequests[0];
        expect(asked.scope).toBe('https://www.googleapis.com/auth/drive.file');
        expect(asked.scope).toBe(DRIVE_FILE_SCOPE);
        expect(asked.client_id).toBe(request.clientId);
        expect(asked.prompt).toBe('');
        expect(asked.login_hint).toBe('pat@example.test');
        expect(asked.include_granted_scopes).toBe(false);
        // Memory only.
        expect(localStorage.setItem).not.toHaveBeenCalled();
        expect(setSession).not.toHaveBeenCalled();
        setSession.mockRestore();
    });

    it('sends no hint when the app does not know who is signed in', async () => {
        const google = fakeGoogle();
        await driveAccessToken({ clientId: request.clientId, loginHint: null }, google.env);
        expect('login_hint' in google.tokenRequests[0]).toBe(false);
    });

    it('hands out the token it holds until shortly before it ends, then asks again', async () => {
        const google = fakeGoogle();
        await driveAccessToken(request, google.env);
        google.clock.now += 3_000_000;
        await driveAccessToken(request, google.env);
        expect(google.tokenRequests).toHaveLength(1);

        google.clock.now += 590_000;
        await driveAccessToken(request, google.env);
        expect(google.tokenRequests).toHaveLength(2);
    });

    it('loads Google\'s script only when it is not on the page yet', async () => {
        const present = fakeGoogle();
        await driveAccessToken(request, present.env);
        expect(present.scriptsLoaded).toEqual([]);

        forgetDriveToken();
        const missing = fakeGoogle({ scriptsMissing: true });
        await driveAccessToken(request, missing.env);
        expect(missing.scriptsLoaded).toEqual(['https://accounts.google.com/gsi/client']);
    });

    it.each([
        ['the person declines', { error: 'access_denied' }],
        ['Google grants something else', { access_token: 't', expires_in: 3600, scope: 'openid email' }],
    ])('refuses when %s, and keeps no token', async (_name, token) => {
        const google = fakeGoogle({ token });
        await expect(driveAccessToken(request, google.env)).rejects.toThrow(/Google Drive access was not given/);
        await expect(driveAccessToken(request, google.env)).rejects.toBeInstanceOf(DriveError);
        expect(google.tokenRequests).toHaveLength(2);
    });

    it.each([
        ['popup_closed', /window was closed/],
        ['popup_failed_to_open', /blocked the Google window/],
        ['unknown', /did not answer/],
    ])('explains %s in plain words', async (errorType, message) => {
        const google = fakeGoogle({ token: { errorType } });
        await expect(driveAccessToken(request, google.env)).rejects.toThrow(message);
    });
});

describe('pickDriveFile', () => {
    const picking = { token: DRIVE_TOKEN, apiKey: 'browser-key', appId: '1234567890' };

    it('opens the picker with the token, the key and the app id, and returns the one file picked', async () => {
        const google = fakeGoogle({ pick: 'picked-id' });

        await expect(pickDriveFile(picking, google.env)).resolves.toBe('picked-id');

        expect(google.pickerCalls).toContainEqual(['gapi.load', 'picker']);
        expect(google.pickerCalls).toContainEqual(['setOAuthToken', DRIVE_TOKEN]);
        expect(google.pickerCalls).toContainEqual(['setDeveloperKey', 'browser-key']);
        expect(google.pickerCalls).toContainEqual(['setAppId', '1234567890']);
        expect(google.pickerCalls).toContainEqual(['setVisible', true]);
        // One file: multi-select is a feature that has to be switched on.
        expect(google.pickerCalls.some(([name]) => name === 'enableFeature')).toBe(false);
        // My Drive and shared drives; folders can be opened but not picked.
        const views = google.pickerCalls.filter(([name]) => name === 'addView').map(([, calls]) => calls as unknown[][]);
        expect(views).toHaveLength(2);
        expect(views[0]).not.toContainEqual(['setEnableDrives', true]);
        expect(views[1]).toContainEqual(['setEnableDrives', true]);
        for (const view of views) expect(view).toContainEqual(['setSelectFolderEnabled', false]);
    });

    it('answers null when the picker is closed without picking', async () => {
        const google = fakeGoogle({ pick: null });
        await expect(pickDriveFile(picking, google.env)).resolves.toBeNull();
    });

    it('loads the API script when it is not on the page yet', async () => {
        const google = fakeGoogle({ scriptsMissing: true });
        await pickDriveFile(picking, google.env);
        expect(google.scriptsLoaded).toEqual(['https://apis.google.com/js/api.js']);
    });
});

describe('readDriveFile', () => {
    beforeEach(() => forgetDriveToken());

    it('asks for the file by id with the token, shared drives included, and changes nothing', async () => {
        const google = fakeGoogle({ drive: () => driveJson(driveFile()) });

        const file = await readDriveFile('file-1', DRIVE_TOKEN, google.env);

        expect(google.driveCalls).toHaveLength(1);
        const call = google.driveCalls[0];
        expect(call.method).toBe('GET');
        const url = new URL(call.url);
        expect(url.origin + url.pathname).toBe('https://www.googleapis.com/drive/v3/files/file-1');
        expect(url.searchParams.get('supportsAllDrives')).toBe('true');
        expect(url.searchParams.get('fields')).toBe('id,name,mimeType,webViewLink,iconLink,permissions(id,type,role,domain,allowFileDiscovery),capabilities(canShare),driveId');
        expect(call.authorization).toBe(`Bearer ${DRIVE_TOKEN}`);
        expect(file).toEqual({
            id: 'file-1',
            name: 'Board plan',
            mimeType: 'application/vnd.google-apps.document',
            webViewLink: 'https://docs.google.com/document/d/file-1/edit?usp=drivesdk',
            sharing: { kind: 'restricted' },
            canShare: true,
        });
    });

    it.each([
        [[{ type: 'user', role: 'owner' }, { type: 'anyone', role: 'reader' }], { kind: 'anyone' }, 'Anyone with the link can view'],
        [[{ type: 'domain', role: 'reader', domain: 'example.org' }], { kind: 'domain', domain: 'example.org' }, 'Only people at example.org with the link'],
        [[{ type: 'user', role: 'owner' }, { type: 'group', role: 'reader' }], { kind: 'restricted' }, 'Restricted: only people it is shared with'],
    ])('reads the sharing from the permissions', async (permissions, sharing, words) => {
        const google = fakeGoogle({ drive: () => driveJson(driveFile({ permissions })) });
        const file = await readDriveFile('file-1', DRIVE_TOKEN, google.env);
        expect(file.sharing).toEqual(sharing);
        expect(describeSharing(file.sharing)).toBe(words);
    });

    it('asks for the permissions list when Google leaves them off the file (a shared drive)', async () => {
        const google = fakeGoogle({
            drive: (call) => call.url.includes('/permissions')
                ? driveJson({ permissions: [{ type: 'anyone', role: 'reader' }] })
                : driveJson(driveFile({ permissions: undefined, driveId: 'drive-9' })),
        });

        const file = await readDriveFile('file-1', DRIVE_TOKEN, google.env);

        expect(file.sharing).toEqual({ kind: 'anyone' });
        expect(google.driveCalls.map(c => c.method)).toEqual(['GET', 'GET']);
        expect(google.driveCalls[1].url).toContain('/files/file-1/permissions?supportsAllDrives=true');
    });

    it('calls the sharing unknown, never restricted or public, when Google will not list it', async () => {
        const google = fakeGoogle({
            drive: (call) => call.url.includes('/permissions')
                ? driveJson({ error: { message: 'Insufficient permissions.' } }, 403)
                : driveJson(driveFile({ permissions: undefined, capabilities: { canShare: false } })),
        });

        const file = await readDriveFile('file-1', DRIVE_TOKEN, google.env);

        expect(file.sharing).toEqual({ kind: 'unknown' });
        expect(file.canShare).toBe(false);
    });

    it('shows Google\'s own message when the file cannot be read', async () => {
        const google = fakeGoogle({ drive: () => driveJson({ error: { message: 'File not found: file-1.' } }, 404) });
        await expect(readDriveFile('file-1', DRIVE_TOKEN, google.env)).rejects.toThrow('Google Drive said: File not found: file-1.');
    });

    it('refuses a folder', async () => {
        const google = fakeGoogle({ drive: () => driveJson(driveFile({ mimeType: 'application/vnd.google-apps.folder' })) });
        await expect(readDriveFile('file-1', DRIVE_TOKEN, google.env)).rejects.toThrow(/folder/);
    });

    it('drops the token Google no longer accepts, so the next click asks again', async () => {
        const google = fakeGoogle({ drive: () => driveJson({ error: { message: 'Invalid Credentials' } }, 401) });
        await driveAccessToken({ clientId: 'c' }, google.env);

        const failure = await readDriveFile('file-1', DRIVE_TOKEN, google.env).catch(e => e as DriveError);
        expect(failure).toBeInstanceOf(DriveError);
        expect((failure as DriveError).unauthorized).toBe(true);

        await driveAccessToken({ clientId: 'c' }, google.env);
        expect(google.tokenRequests).toHaveLength(2);
    });
});

describe('sharingOf', () => {
    it('is unknown without a list, and "anyone" wins over a domain', () => {
        expect(sharingOf(null)).toEqual({ kind: 'unknown' });
        expect(sharingOf([{ type: 'domain', domain: 'example.org' }, { type: 'anyone' }])).toEqual({ kind: 'anyone' });
        expect(sharingOf([])).toEqual({ kind: 'restricted' });
    });
});

describe('shareWithAnyone', () => {
    it('adds exactly one permission: anyone, reader', async () => {
        const google = fakeGoogle({ drive: () => driveJson({ id: 'anyoneWithLink' }) });

        await shareWithAnyone('file-1', DRIVE_TOKEN, google.env);

        expect(google.driveCalls).toEqual([{
            method: 'POST',
            url: 'https://www.googleapis.com/drive/v3/files/file-1/permissions?supportsAllDrives=true',
            body: { type: 'anyone', role: 'reader' },
            authorization: `Bearer ${DRIVE_TOKEN}`,
        }]);
    });

    it('throws Google\'s own words when Google refuses', async () => {
        const google = fakeGoogle({
            drive: () => driveJson({ error: { message: 'The domain administrators have disabled sharing outside the domain.' } }, 403),
        });
        await expect(shareWithAnyone('file-1', DRIVE_TOKEN, google.env)).rejects.toThrow(
            'Google Drive would not change the sharing: The domain administrators have disabled sharing outside the domain.',
        );
    });
});

describe('emailFromToken', () => {
    const token = (payload: object) => `h.${btoa(JSON.stringify(payload)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')}.s`;

    it('reads the email from one of our sign-in tokens', () => {
        expect(emailFromToken(token({ sub: 'pat@example.test', user_id: 4 }))).toBe('pat@example.test');
    });

    it('answers null for anything else', () => {
        expect(emailFromToken(null)).toBeNull();
        expect(emailFromToken('opn_apikey')).toBeNull();
        expect(emailFromToken('a.%%%.c')).toBeNull();
        expect(emailFromToken(token({ sub: 'not-an-email' }))).toBeNull();
    });
});
