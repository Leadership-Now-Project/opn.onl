import { beforeEach, describe, expect, it, vi } from 'vitest';
import { DRIVE_TOKEN, driveFile, driveJson, fakeGoogle } from '../test/fakeGoogle';
import { cachedDriveToken, forgetDriveToken as forgetHeldToken } from './driveToken';
import {
    cleanDriveMimeType,
    cleanDriveName,
    DRIVE_FILE_SCOPE,
    DRIVE_LINK_LIMITS,
    describeSharing,
    driveAccessToken,
    DriveError,
    driveLinkProblem,
    emailFromToken,
    forgetDriveToken,
    isDriveLinkUrl,
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

describe('the token held in memory', () => {
    beforeEach(() => forgetDriveToken());

    it('is the one the sign-out module drops: after that Google is asked again', async () => {
        const google = fakeGoogle();
        await driveAccessToken(request, google.env);
        expect(cachedDriveToken()?.token).toBe(DRIVE_TOKEN);

        // What Layout, Settings and authFetch call on the way out of a session.
        forgetHeldToken();

        expect(cachedDriveToken()).toBeNull();
        await driveAccessToken(request, google.env);
        expect(google.tokenRequests).toHaveLength(2);
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

    it.each([
        ['cannot be built', { pickerThrows: true }],
        ['reports an error', { pickerErrors: true }],
    ])('rejects with words for the person when the picker %s', async (_name, options) => {
        const google = fakeGoogle(options);
        const failure = await pickDriveFile(picking, google.env).catch(e => e as Error);
        expect(failure).toBeInstanceOf(DriveError);
        expect((failure as Error).message).toBe('Google\'s file picker could not be opened. Click Google Drive Files to try again.');
    });

    it('says when the picker has been shown, and a newer picker closes the one before it', async () => {
        const google = fakeGoogle({ pickerSilent: true });
        const shown = vi.fn();

        const first = pickDriveFile({ ...picking, onShown: shown }, google.env);
        await vi.waitFor(() => expect(shown).toHaveBeenCalledTimes(1));
        expect(google.pickerCalls).toContainEqual(['setVisible', true]);

        const second = pickDriveFile(picking, google.env);
        // The first is closed and answers "nothing picked".
        await expect(first).resolves.toBeNull();
        expect(google.pickerCalls).toContainEqual(['setVisible', false]);
        expect(google.pickerCalls).toContainEqual(['dispose']);

        // Its late answer changes nothing; the second still works.
        google.answerPicker(0, 'late');
        google.answerPicker(1, 'picked-id');
        await expect(second).resolves.toBe('picked-id');
    });

    it('does not say the picker was shown when it could not be built', async () => {
        const google = fakeGoogle({ pickerThrows: true });
        const shown = vi.fn();
        await expect(pickDriveFile({ ...picking, onShown: shown }, google.env)).rejects.toBeInstanceOf(DriveError);
        expect(shown).not.toHaveBeenCalled();
    });
});

// These mirror `DriveFileRef::checked` and `is_drive_url` in
// backend/src/utils/drive.rs (and its unit tests): what passes here must pass
// there, because the sharing change is made on the strength of it.
describe('what our server accepts for a Drive file', () => {
    it('has the backend\'s limits', () => {
        expect(DRIVE_LINK_LIMITS).toEqual({ fileIdLength: 200, nameChars: 500, mimeTypeBytes: 255, titleChars: 500 });
    });

    it.each([
        'https://docs.google.com/document/d/abc/edit',
        'https://drive.google.com/file/d/abc/view',
        'https://drive.google.com:443/file/d/abc/view',
    ])('takes %s as a Drive address', (url) => {
        expect(isDriveLinkUrl(url)).toBe(true);
    });

    it.each([
        'http://drive.google.com/file/d/abc/view',
        'https://drive.google.com.evil.test/file/d/abc',
        'https://evil.test/?u=https://drive.google.com/',
        'https://user@evil.test:443@drive.google.com.evil.test/',
        'https://drive.google.com:8443/file/d/abc',
        'https://sites.google.com/view/abc',
        'https://script.google.com/d/abc/edit',
        'https://colab.research.google.com/drive/abc',
        'https://www.google.com/maps/d/edit?mid=abc',
        'https://drive.google.com./file/d/abc',
        'https://user@drive.google.com/file/d/abc',
        'https://drive.google.com/file/d/a\tbc',
        'https://drive.google.com/file/d/abc\n',
        'opn-file:12',
        'not a url',
        '',
    ])('refuses %j as a Drive address', (url) => {
        expect(isDriveLinkUrl(url)).toBe(false);
    });

    it('finds no problem with a Drive file on a Drive address', () => {
        expect(driveLinkProblem({ id: '1AbC_d-9', webViewLink: 'https://docs.google.com/document/d/1AbC_d-9/edit' })).toBeNull();
        expect(driveLinkProblem({ id: 'a'.repeat(200), webViewLink: 'https://drive.google.com/file/d/x/view' })).toBeNull();
    });

    it('says a file on another host cannot be shared from here yet', () => {
        expect(driveLinkProblem({ id: 'abc', webViewLink: 'https://script.google.com/d/abc/edit' }))
            .toBe('This kind of Google file cannot be shared from here yet.');
    });

    it.each(['', 'abc/def', 'abc def', 'a'.repeat(201), 'abc\n'])('refuses the file id %j', (id) => {
        expect(driveLinkProblem({ id, webViewLink: 'https://docs.google.com/document/d/x/edit' }))
            .toBe('This file cannot be shared from here: Google Drive gave it an id this app cannot store.');
    });

    it('cleans a name so it always passes: no control characters, 1 to 500 characters', () => {
        expect(cleanDriveName(' Plan.pdf ')).toBe('Plan.pdf');
        expect(cleanDriveName('a\tb\nc\u0000d\u007fe\u0085f')).toBe('abcdef');
        expect(Array.from(cleanDriveName('n'.repeat(501)))).toHaveLength(500);
        // Counted in characters, as the server counts them, not in UTF-16 units.
        expect(Array.from(cleanDriveName('\u{1F600}'.repeat(501)))).toHaveLength(500);
        expect(cleanDriveName(`${'n'.repeat(499)} tail`)).toBe('n'.repeat(499));
        expect(cleanDriveName('')).toBe('Untitled');
        expect(cleanDriveName(undefined)).toBe('Untitled');
        expect(cleanDriveName(' \t\n ')).toBe('Untitled');
    });

    it('cleans a type so it always passes: no control characters, at most 255 bytes', () => {
        expect(cleanDriveMimeType('application/pdf')).toBe('application/pdf');
        expect(cleanDriveMimeType(' text/\u0007plain\n')).toBe('text/plain');
        expect(cleanDriveMimeType('m'.repeat(256))).toBe('m'.repeat(255));
        // Bytes, as the server counts them: 127 two-byte characters fit, 128 do not.
        expect(cleanDriveMimeType('é'.repeat(200))).toBe('é'.repeat(127));
        // Drive does not always name a type.
        expect(cleanDriveMimeType(undefined)).toBe('');
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
        expect(url.searchParams.get('fields')).toBe('id,name,mimeType,webViewLink,permissions(id,type,role,domain,allowFileDiscovery),capabilities(canShare)');
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
        [[{ type: 'user', role: 'owner' }, { type: 'anyone', role: 'reader' }], { kind: 'anyone', access: 'view' }, 'Anyone with the link can view'],
        [[{ type: 'user', role: 'owner' }, { type: 'anyone', role: 'commenter' }], { kind: 'anyone', access: 'comment' }, 'Anyone with the link can comment'],
        [[{ type: 'user', role: 'owner' }, { type: 'anyone', role: 'writer' }], { kind: 'anyone', access: 'edit' }, 'Anyone with the link can edit'],
        [[{ type: 'anyone', role: 'reader' }, { type: 'anyone', role: 'writer' }, { type: 'anyone', role: 'commenter' }], { kind: 'anyone', access: 'edit' }, 'Anyone with the link can edit'],
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

        expect(file.sharing).toEqual({ kind: 'anyone', access: 'view' });
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

    it('hands back a name and a type our server will take, whatever Drive holds', async () => {
        const google = fakeGoogle({
            drive: () => driveJson(driveFile({ name: `Board\tplan\n${'n'.repeat(600)}`, mimeType: 'application/\u0007pdf' })),
        });

        const file = await readDriveFile('file-1', DRIVE_TOKEN, google.env);

        expect(file.name).toBe(`Boardplan${'n'.repeat(491)}`);
        expect(file.mimeType).toBe('application/pdf');
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
        expect(sharingOf([{ type: 'domain', domain: 'example.org' }, { type: 'anyone' }])).toEqual({ kind: 'anyone', access: 'view' });
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
