// Google Drive from the browser: an access token for the files a person picks,
// Google's file picker, and the two Drive calls the Files tab makes (read a
// file's sharing; make it "anyone with the link"). Nothing here runs until
// someone clicks "Google Drive Files". The token stays in this module's
// memory: it is never put in storage and never sent to our own API.
//
// https://developers.google.com/identity/oauth2/web/guides/use-token-model
// https://developers.google.com/workspace/drive/picker/guides/web-picker

/** The one scope asked for: files the person opens or picks with this app. */
export const DRIVE_FILE_SCOPE = 'https://www.googleapis.com/auth/drive.file';

const GSI_SCRIPT_URL = 'https://accounts.google.com/gsi/client';
const GAPI_SCRIPT_URL = 'https://apis.google.com/js/api.js';
const DRIVE_API = 'https://www.googleapis.com/drive/v3';
const FILE_FIELDS = 'id,name,mimeType,webViewLink,iconLink,permissions(id,type,role,domain,allowFileDiscovery),capabilities(canShare),driveId';
const PERMISSION_FIELDS = 'nextPageToken,permissions(id,type,role,domain,allowFileDiscovery)';
const FOLDER_MIME_TYPE = 'application/vnd.google-apps.folder';
/** A token this close to its end is not handed out again. */
const TOKEN_MARGIN_MS = 60_000;
/** Pages of a file's permissions read before giving up on finding "anyone". */
const MAX_PERMISSION_PAGES = 10;

interface TokenResponse {
    access_token?: string;
    expires_in?: number | string;
    scope?: string;
    error?: string;
}

interface TokenClientConfig {
    client_id: string;
    scope: string;
    prompt: string;
    include_granted_scopes: boolean;
    login_hint?: string;
    callback: (response: TokenResponse) => void;
    error_callback: (error: { type?: string }) => void;
}

interface PickerView {
    setIncludeFolders(included: boolean): PickerView;
    setSelectFolderEnabled(enabled: boolean): PickerView;
    setEnableDrives(enabled: boolean): PickerView;
    setMode(mode: unknown): PickerView;
}

interface PickerBuilder {
    addView(view: PickerView): PickerBuilder;
    setOAuthToken(token: string): PickerBuilder;
    setDeveloperKey(key: string): PickerBuilder;
    setAppId(appId: string): PickerBuilder;
    setCallback(callback: (data: Record<string, unknown>) => void): PickerBuilder;
    build(): { setVisible(visible: boolean): void };
}

interface PickerApi {
    PickerBuilder: new () => PickerBuilder;
    DocsView: new (viewId?: unknown) => PickerView;
    ViewId: { DOCS: unknown };
    DocsViewMode: { LIST: unknown };
    Action: { PICKED: string; CANCEL: string };
    Response: { ACTION: string; DOCUMENTS: string };
    Document: { ID: string };
}

export interface GoogleApis {
    accounts?: { oauth2?: { initTokenClient: (config: TokenClientConfig) => { requestAccessToken: () => void } } };
    picker?: PickerApi;
}

export interface Gapi {
    load: (name: string, options: { callback: () => void; onerror: () => void; timeout: number; ontimeout: () => void }) => void;
}

/** What this module reaches outside itself through, so a test can stand in for Google. */
export interface DriveEnv {
    fetch: typeof fetch;
    loadScript: (url: string) => Promise<void>;
    google: () => GoogleApis | undefined;
    gapi: () => Gapi | undefined;
    now: () => number;
}

const scripts = new Map<string, Promise<void>>();

function loadScript(url: string): Promise<void> {
    let loading = scripts.get(url);
    if (!loading) {
        loading = new Promise<void>((resolve, reject) => {
            const script = document.createElement('script');
            script.src = url;
            script.async = true;
            script.onload = () => resolve();
            script.onerror = () => {
                // Let the next click try again.
                scripts.delete(url);
                script.remove();
                reject(new Error('load failed'));
            };
            document.head.appendChild(script);
        });
        scripts.set(url, loading);
    }
    return loading;
}

export const browserDriveEnv: DriveEnv = {
    fetch: (input, init) => window.fetch(input, init),
    loadScript,
    google: () => (window as unknown as { google?: GoogleApis }).google,
    gapi: () => (window as unknown as { gapi?: Gapi }).gapi,
    now: () => Date.now(),
};

/** Something went wrong that the person should read. `message` is written for them. */
export class DriveError extends Error {
    /** The access token was refused: the next click asks Google for a new one. */
    readonly unauthorized: boolean;
    constructor(message: string, unauthorized = false) {
        super(message);
        this.name = 'DriveError';
        this.unauthorized = unauthorized;
    }
}

const LOAD_FAILED = 'Could not load Google Drive. Check your connection and try again.';

let cachedToken: { clientId: string; token: string; expiresAt: number } | null = null;

/** Drop the token held in memory, so the next request asks Google again. */
export function forgetDriveToken(): void {
    cachedToken = null;
}

/**
 * An access token for the files the person picks, and nothing else in their
 * Drive. Opens Google's window only when there is no unexpired token in
 * memory; someone who has already agreed is not asked again.
 */
export async function driveAccessToken(
    request: { clientId: string; loginHint?: string | null },
    env: DriveEnv = browserDriveEnv,
): Promise<string> {
    if (cachedToken && cachedToken.clientId === request.clientId && cachedToken.expiresAt - TOKEN_MARGIN_MS > env.now()) {
        return cachedToken.token;
    }
    cachedToken = null;
    if (!env.google()?.accounts?.oauth2) {
        await env.loadScript(GSI_SCRIPT_URL).catch(() => { throw new DriveError(LOAD_FAILED); });
    }
    const oauth2 = env.google()?.accounts?.oauth2;
    if (!oauth2) throw new DriveError(LOAD_FAILED);

    const response = await new Promise<TokenResponse>((resolve, reject) => {
        const config: TokenClientConfig = {
            client_id: request.clientId,
            scope: DRIVE_FILE_SCOPE,
            prompt: '',
            // Only Drive files, not whatever else this person granted at sign-in.
            include_granted_scopes: false,
            callback: resolve,
            error_callback: (error) => reject(new DriveError(
                error?.type === 'popup_closed' ? 'The Google window was closed before it finished. Please try again.'
                : error?.type === 'popup_failed_to_open' ? 'Your browser blocked the Google window. Allow pop-ups for this site and try again.'
                : 'Google did not answer. Please try again.',
            )),
        };
        if (request.loginHint) config.login_hint = request.loginHint;
        oauth2.initTokenClient(config).requestAccessToken();
    });

    const granted = (response.scope ?? '').split(' ').includes(DRIVE_FILE_SCOPE);
    if (response.error || !response.access_token || !granted) {
        throw new DriveError('Google Drive access was not given, so no file can be picked. Click Google Drive Files to try again.');
    }
    const lifetime = Number(response.expires_in);
    cachedToken = {
        clientId: request.clientId,
        token: response.access_token,
        expiresAt: env.now() + (Number.isFinite(lifetime) ? lifetime : 0) * 1000,
    };
    return response.access_token;
}

/**
 * Open Google's file picker over the page. Resolves with the id of the one
 * file picked, or `null` when the person closes the picker without picking.
 */
export async function pickDriveFile(
    request: { token: string; apiKey: string; appId: string },
    env: DriveEnv = browserDriveEnv,
): Promise<string | null> {
    if (!env.google()?.picker) {
        if (!env.gapi()) {
            await env.loadScript(GAPI_SCRIPT_URL).catch(() => { throw new DriveError(LOAD_FAILED); });
        }
        const gapi = env.gapi();
        if (!gapi) throw new DriveError(LOAD_FAILED);
        await new Promise<void>((resolve, reject) => {
            const failed = () => reject(new DriveError(LOAD_FAILED));
            gapi.load('picker', { callback: resolve, onerror: failed, timeout: 15_000, ontimeout: failed });
        });
    }
    const picker = env.google()?.picker;
    if (!picker) throw new DriveError(LOAD_FAILED);

    return new Promise<string | null>((resolve) => {
        // List mode: with the drive.file scope Google shows no thumbnails.
        // Folders are shown to browse into, never to pick.
        const view = () => new picker.DocsView(picker.ViewId.DOCS)
            .setIncludeFolders(true)
            .setSelectFolderEnabled(false)
            .setMode(picker.DocsViewMode.LIST);
        // My Drive, then shared drives. One file at a time:
        // MULTISELECT_ENABLED is left off.
        new picker.PickerBuilder()
            .addView(view())
            .addView(view().setEnableDrives(true))
            .setOAuthToken(request.token)
            .setDeveloperKey(request.apiKey)
            .setAppId(request.appId)
            .setCallback((data) => {
                const action = data[picker.Response.ACTION];
                if (action === picker.Action.PICKED) {
                    const documents = data[picker.Response.DOCUMENTS] as Record<string, unknown>[] | undefined;
                    const id = documents?.[0]?.[picker.Document.ID];
                    resolve(typeof id === 'string' && id ? id : null);
                } else if (action === picker.Action.CANCEL) {
                    resolve(null);
                }
            })
            .build()
            .setVisible(true);
    });
}

export interface DrivePermission {
    id?: string;
    type?: string;
    role?: string;
    domain?: string;
    allowFileDiscovery?: boolean;
}

/** Who can open a Drive file with its link, as far as Google told us. */
export type DriveSharing =
    | { kind: 'anyone' }
    | { kind: 'domain'; domain: string }
    | { kind: 'restricted' }
    /** Google would not list the file's permissions to this person. */
    | { kind: 'unknown' };

export interface DriveFile {
    id: string;
    name: string;
    mimeType: string;
    webViewLink: string;
    sharing: DriveSharing;
    /** Whether this person may change the file's sharing. */
    canShare: boolean;
}

export function sharingOf(permissions: DrivePermission[] | null): DriveSharing {
    if (!permissions) return { kind: 'unknown' };
    if (permissions.some(p => p.type === 'anyone')) return { kind: 'anyone' };
    const domain = permissions.find(p => p.type === 'domain');
    if (domain) return { kind: 'domain', domain: domain.domain ?? '' };
    return { kind: 'restricted' };
}

/** The file's sharing in plain words. */
export function describeSharing(sharing: DriveSharing): string {
    switch (sharing.kind) {
        case 'anyone': return 'Anyone with the link can view';
        case 'domain': return `Only people at ${sharing.domain || 'your organization'} with the link`;
        case 'restricted': return 'Restricted: only people it is shared with';
        case 'unknown': return 'Sharing could not be read for this file';
    }
}

/** Google's own explanation of a refused call, or `fallback` when it sent none. */
async function googleMessage(response: Response, fallback: string): Promise<string> {
    const body = await response.json().catch(() => null) as { error?: { message?: unknown } } | null;
    const message = body?.error?.message;
    return typeof message === 'string' && message.trim() ? message.trim() : fallback;
}

async function driveGet(url: string, token: string, env: DriveEnv): Promise<Response> {
    let response: Response;
    try {
        response = await env.fetch(url, { headers: { Authorization: `Bearer ${token}` } });
    } catch {
        throw new DriveError('Could not reach Google Drive. Check your connection and try again.');
    }
    if (response.status === 401) {
        forgetDriveToken();
        throw new DriveError('Your Google Drive session ended. Click Google Drive Files to try again.', true);
    }
    return response;
}

/**
 * Read the picked file: its name, where it opens, and who can open it. Reads
 * only; nothing about the file changes.
 */
export async function readDriveFile(fileId: string, token: string, env: DriveEnv = browserDriveEnv): Promise<DriveFile> {
    const id = encodeURIComponent(fileId);
    const response = await driveGet(`${DRIVE_API}/files/${id}?supportsAllDrives=true&fields=${encodeURIComponent(FILE_FIELDS)}`, token, env);
    if (!response.ok) {
        throw new DriveError(`Google Drive said: ${await googleMessage(response, `the file could not be read (${response.status}).`)}`);
    }
    const file = await response.json() as {
        id?: string; name?: string; mimeType?: string; webViewLink?: string;
        permissions?: DrivePermission[]; capabilities?: { canShare?: boolean };
    };
    if (file.mimeType === FOLDER_MIME_TYPE) {
        throw new DriveError('That is a folder. Pick a file to share.');
    }
    if (!file.id || !file.webViewLink) {
        throw new DriveError('Google Drive gave no link for this file, so it cannot be shared here.');
    }

    // Google leaves `permissions` out for a file in a shared drive, and for
    // one this person cannot share. The permissions list is asked instead; if
    // that is refused too, the sharing is unknown, never assumed.
    let permissions: DrivePermission[] | null = Array.isArray(file.permissions) ? file.permissions : null;
    if (!permissions) permissions = await listPermissions(id, token, env);

    return {
        id: file.id,
        name: file.name || 'Untitled',
        mimeType: file.mimeType ?? '',
        webViewLink: file.webViewLink,
        sharing: sharingOf(permissions),
        canShare: file.capabilities?.canShare === true,
    };
}

async function listPermissions(encodedId: string, token: string, env: DriveEnv): Promise<DrivePermission[] | null> {
    const all: DrivePermission[] = [];
    let pageToken = '';
    for (let page = 0; page < MAX_PERMISSION_PAGES; page += 1) {
        const query = `supportsAllDrives=true&pageSize=100&fields=${encodeURIComponent(PERMISSION_FIELDS)}${pageToken ? `&pageToken=${encodeURIComponent(pageToken)}` : ''}`;
        const response = await driveGet(`${DRIVE_API}/files/${encodedId}/permissions?${query}`, token, env);
        if (!response.ok) return null;
        const body = await response.json() as { permissions?: DrivePermission[]; nextPageToken?: string };
        all.push(...(body.permissions ?? []));
        if (!body.nextPageToken) return all;
        pageToken = body.nextPageToken;
    }
    // More pages than are read: only a found "anyone" is certain.
    return all.some(p => p.type === 'anyone') ? all : null;
}

/**
 * Change the file to "anyone with the link can view". The only request in the
 * app that changes anything in Drive; the caller sends it only after the
 * person ticked the box and clicked Create. Throws Google's own words when
 * Google refuses.
 */
export async function shareWithAnyone(fileId: string, token: string, env: DriveEnv = browserDriveEnv): Promise<void> {
    let response: Response;
    try {
        response = await env.fetch(`${DRIVE_API}/files/${encodeURIComponent(fileId)}/permissions?supportsAllDrives=true`, {
            method: 'POST',
            headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
            body: JSON.stringify({ type: 'anyone', role: 'reader' }),
        });
    } catch {
        throw new DriveError('Could not reach Google Drive, so the sharing was not changed. Check your connection and try again.');
    }
    if (response.status === 401) {
        forgetDriveToken();
        throw new DriveError('Your Google Drive session ended, so the sharing was not changed. Click Google Drive Files to try again.', true);
    }
    if (!response.ok) {
        const reason = await googleMessage(response, `the request was refused (${response.status}).`);
        throw new DriveError(`Google Drive would not change the sharing: ${reason}`);
    }
}

/**
 * The email in one of our own sign-in tokens, as a hint for which Google
 * account to use. `null` when the token is not one we can read; Google then
 * shows its account chooser.
 */
export function emailFromToken(token: string | null): string | null {
    const payload = token?.split('.')[1];
    if (!payload) return null;
    try {
        const base64 = payload.replace(/-/g, '+').replace(/_/g, '/');
        const bytes = Uint8Array.from(atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, '=')), c => c.charCodeAt(0));
        const sub = (JSON.parse(new TextDecoder().decode(bytes)) as { sub?: unknown }).sub;
        return typeof sub === 'string' && sub.includes('@') ? sub : null;
    } catch {
        return null;
    }
}
