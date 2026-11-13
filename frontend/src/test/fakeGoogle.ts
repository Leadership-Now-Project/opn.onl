import { vi } from 'vitest';
import type { DriveEnv, GoogleApis } from '../utils/googleDrive';

// Google, as far as the Drive code reaches it: the token client, the file
// picker and the Drive API. A test says what each answers and reads back what
// was asked. No test talks to the real thing.

export const DRIVE_TOKEN = 'ya29.drive-access-token-for-tests';

export interface DriveCall {
    method: string;
    url: string;
    body: unknown;
    authorization: string | null;
}

export interface FakeGoogle {
    env: DriveEnv;
    /** Every config a token client was made with. */
    tokenRequests: Record<string, unknown>[];
    /** Every call the picker's builder received, in order. */
    pickerCalls: [string, ...unknown[]][];
    /** Every request to the Drive API, in order. */
    driveCalls: DriveCall[];
    scriptsLoaded: string[];
    clock: { now: number };
    /** Make a picker that is still open answer now: 0 is the first one opened. */
    answerPicker: (index: number, pick: string | null) => void;
    /** The options in use; a test may change them between two clicks. */
    options: FakeGoogleOptions;
}

export interface FakeGoogleOptions {
    /** What Google's token popup answers. Default: a token for drive.file. */
    token?: Record<string, unknown> | { errorType: string };
    /** The file id the person picks; `null` closes the picker without picking. */
    pick?: string | null;
    /** The file ids picked by the first, second, ... picker; then `pick`. */
    picks?: (string | null)[];
    /** The picker opens and never answers (its frame did not load). */
    pickerSilent?: boolean;
    /** Building the picker throws. */
    pickerThrows?: boolean;
    /** The picker reports an error instead of a pick. */
    pickerErrors?: boolean;
    /** The Drive API. Default: every call is a 404. */
    drive?: (call: DriveCall) => { status: number; body: unknown };
    /** Start with Google's scripts not yet on the page. */
    scriptsMissing?: boolean;
    /** Called for every Drive request, before it is answered (to record order). */
    onDriveCall?: (call: DriveCall) => void;
}

export function driveJson(body: unknown, status = 200) {
    return { status, body };
}

export function fakeGoogle(options: FakeGoogleOptions = {}): FakeGoogle {
    const tokenRequests: Record<string, unknown>[] = [];
    const pickerCalls: [string, ...unknown[]][] = [];
    const driveCalls: DriveCall[] = [];
    const scriptsLoaded: string[] = [];
    const clock = { now: 1_000_000 };
    const openPickers: ((data: Record<string, unknown>) => void)[] = [];
    const answer = (pick: string | null) => pick === null
        ? { action: 'cancel' }
        : { action: 'picked', docs: [{ id: pick, name: 'ignored' }] };

    class DocsView {
        calls: [string, ...unknown[]][] = [];
        constructor(viewId: unknown) { this.calls.push(['DocsView', viewId]); }
        setIncludeFolders(v: boolean) { this.calls.push(['setIncludeFolders', v]); return this; }
        setSelectFolderEnabled(v: boolean) { this.calls.push(['setSelectFolderEnabled', v]); return this; }
        setEnableDrives(v: boolean) { this.calls.push(['setEnableDrives', v]); return this; }
        setMode(v: unknown) { this.calls.push(['setMode', v]); return this; }
    }

    class PickerBuilder {
        private callback: (data: Record<string, unknown>) => void = () => {};
        addView(view: DocsView) { pickerCalls.push(['addView', view.calls]); return this; }
        setOAuthToken(token: string) { pickerCalls.push(['setOAuthToken', token]); return this; }
        setDeveloperKey(key: string) { pickerCalls.push(['setDeveloperKey', key]); return this; }
        setAppId(appId: string) { pickerCalls.push(['setAppId', appId]); return this; }
        enableFeature(feature: unknown) { pickerCalls.push(['enableFeature', feature]); return this; }
        setCallback(callback: (data: Record<string, unknown>) => void) { this.callback = callback; return this; }
        build() {
            if (options.pickerThrows) throw new Error('The API developer key is invalid.');
            return {
                setVisible: (visible: boolean) => {
                    pickerCalls.push(['setVisible', visible]);
                    if (!visible) return;
                    const index = openPickers.push(this.callback) - 1;
                    // Google first reports that the picker loaded.
                    this.callback({ action: 'loaded' });
                    if (options.pickerSilent) return;
                    if (options.pickerErrors) {
                        queueMicrotask(() => this.callback({ action: 'error' }));
                        return;
                    }
                    const queued = options.picks && index < options.picks.length ? options.picks[index] : options.pick;
                    const pick = queued === undefined ? 'file-1' : queued;
                    queueMicrotask(() => this.callback(answer(pick)));
                },
                dispose: () => { pickerCalls.push(['dispose']); },
            };
        }
    }

    const apis: GoogleApis = {};
    const oauth2 = {
        initTokenClient: (config: Record<string, unknown>) => {
            tokenRequests.push(config);
            return {
                requestAccessToken: () => {
                    const answer = options.token ?? {
                        access_token: DRIVE_TOKEN,
                        expires_in: 3600,
                        scope: 'https://www.googleapis.com/auth/drive.file',
                    };
                    queueMicrotask(() => {
                        if ('errorType' in answer) (config.error_callback as (e: unknown) => void)({ type: answer.errorType });
                        else (config.callback as (r: unknown) => void)(answer);
                    });
                },
            };
        },
    };
    const picker = {
        PickerBuilder,
        DocsView,
        ViewId: { DOCS: 'all' },
        DocsViewMode: { LIST: 'list' },
        Action: { PICKED: 'picked', CANCEL: 'cancel' },
        Response: { ACTION: 'action', DOCUMENTS: 'docs' },
        Document: { ID: 'id' },
    };
    let gapi: { load: (name: string, options: { callback: () => void }) => void } | undefined;
    const installGapi = () => {
        gapi = {
            load: (name, loadOptions) => {
                pickerCalls.push(['gapi.load', name]);
                apis.picker = picker as unknown as GoogleApis['picker'];
                loadOptions.callback();
            },
        };
    };
    if (!options.scriptsMissing) {
        apis.accounts = { oauth2: oauth2 as unknown as NonNullable<GoogleApis['accounts']>['oauth2'] };
        installGapi();
    }

    const env: DriveEnv = {
        fetch: vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
            const headers = new Headers(init?.headers);
            const call: DriveCall = {
                method: init?.method ?? 'GET',
                url: String(input),
                body: init?.body ? JSON.parse(String(init.body)) : undefined,
                authorization: headers.get('Authorization'),
            };
            driveCalls.push(call);
            options.onDriveCall?.(call);
            const answer = options.drive?.(call) ?? driveJson({ error: { message: 'File not found.' } }, 404);
            return {
                ok: answer.status >= 200 && answer.status < 300,
                status: answer.status,
                json: () => Promise.resolve(answer.body),
            } as Response;
        }) as unknown as typeof fetch,
        loadScript: async (url: string) => {
            scriptsLoaded.push(url);
            if (url.includes('accounts.google.com')) {
                apis.accounts = { oauth2: oauth2 as unknown as NonNullable<GoogleApis['accounts']>['oauth2'] };
            } else {
                installGapi();
            }
        },
        google: () => apis,
        gapi: () => gapi as ReturnType<DriveEnv['gapi']>,
        now: () => clock.now,
    };

    const answerPicker = (index: number, pick: string | null) => openPickers[index](answer(pick));

    return { env, tokenRequests, pickerCalls, driveCalls, scriptsLoaded, clock, answerPicker, options };
}

/** A Drive file as `files.get` answers, with the given permissions. */
export function driveFile(overrides: Record<string, unknown> = {}) {
    return {
        id: 'file-1',
        name: 'Board plan',
        mimeType: 'application/vnd.google-apps.document',
        webViewLink: 'https://docs.google.com/document/d/file-1/edit?usp=drivesdk',
        permissions: [{ id: 'u1', type: 'user', role: 'owner' }],
        capabilities: { canShare: true },
        ...overrides,
    };
}
