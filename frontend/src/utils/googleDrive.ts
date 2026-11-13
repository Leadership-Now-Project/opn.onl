// Google Drive from the browser: an access token for the files a person picks,
// Google's file picker, and the two Drive calls the Files tab makes (read a
// file's sharing; make it "anyone with the link"). Nothing here runs until
// someone clicks "Google Drive Files". The token stays in this module's
// memory: it is never put in storage and never sent to our own API.
//
// https://developers.google.com/identity/oauth2/web/guides/use-token-model
// https://developers.google.com/workspace/drive/picker/guides/web-picker

import { cachedDriveToken, forgetDriveToken, rememberDriveToken } from './driveToken';

export { forgetDriveToken };

/** The one scope asked for: files the person opens or picks with this app. */
export const DRIVE_FILE_SCOPE = 'https://www.googleapis.com/auth/drive.file';

const GSI_SCRIPT_URL = 'https://accounts.google.com/gsi/client';
const GAPI_SCRIPT_URL = 'https://apis.google.com/js/api.js';
const DRIVE_API = 'https://www.googleapis.com/drive/v3';
const FILE_FIELDS = 'id,name,mimeType,webViewLink,permissions(id,type,role,domain,allowFileDiscovery),capabilities(canShare)';
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
    build(): BuiltPicker;
}

interface BuiltPicker {
    setVisible(visible: boolean): void;
    dispose?: () => void;
}

interface PickerApi {
    PickerBuilder: new () => PickerBuilder;
    DocsView: new (viewId?: unknown) => PickerView;
    ViewId: { DOCS: unknown; DOCS_VIDEOS?: unknown };
    DocsViewMode: { LIST: unknown };
    Action: { PICKED: string; CANCEL: string; ERROR?: string };
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
const PICKER_FAILED = 'Google\'s file picker could not be opened. Click Google Drive Files to try again.';

/**
 * An access token for the files the person picks, and nothing else in their
 * Drive. Opens Google's window only when there is no unexpired token in
 * memory; someone who has already agreed is not asked again.
 */
export async function driveAccessToken(
    request: { clientId: string; loginHint?: string | null },
    env: DriveEnv = browserDriveEnv,
): Promise<string> {
    const cached = cachedDriveToken();
    if (cached && cached.clientId === request.clientId && cached.expiresAt - TOKEN_MARGIN_MS > env.now()) {
        return cached.token;
    }
    forgetDriveToken();
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
    rememberDriveToken({
        clientId: request.clientId,
        token: response.access_token,
        expiresAt: env.now() + (Number.isFinite(lifetime) ? lifetime : 0) * 1000,
    });
    return response.access_token;
}

/** Gives up on the picker that is open, when a newer one replaces it. */
let abandonOpenPicker: (() => void) | null = null;

/**
 * Open Google's file picker over the page. Resolves with the id of the one
 * file picked, or `null` when the person closes the picker without picking.
 *
 * Google gives no signal when the picker's frame fails to appear, and a
 * person may browse it for as long as they like, so nothing here times out.
 * Instead `onShown` is called once the picker has been asked to show: from
 * then on the caller can let the person click again. A second call closes
 * the earlier picker, whose promise resolves `null`. A picker that cannot be
 * built, or that reports an error, rejects with words for the person.
 *
 * `videosOnly` asks Google for its videos view (the Media tab). That is a
 * convenience for the person, not a rule: the caller still checks the type of
 * whatever comes back.
 */
export async function pickDriveFile(
    request: { token: string; apiKey: string; appId: string; videosOnly?: boolean; onShown?: () => void },
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

    abandonOpenPicker?.();

    return new Promise<string | null>((resolve, reject) => {
        let built: BuiltPicker | null = null;
        let settled = false;
        const settle = (finish: () => void) => {
            if (settled) return;
            settled = true;
            if (abandonOpenPicker === abandon) abandonOpenPicker = null;
            finish();
        };
        const abandon = () => settle(() => {
            try {
                built?.setVisible(false);
                built?.dispose?.();
            } catch {
                // It is gone already.
            }
            resolve(null);
        });
        try {
            // List mode: with the drive.file scope Google shows no thumbnails.
            // Folders are shown to browse into, never to pick.
            const viewId = request.videosOnly ? picker.ViewId.DOCS_VIDEOS ?? picker.ViewId.DOCS : picker.ViewId.DOCS;
            const view = () => new picker.DocsView(viewId)
                .setIncludeFolders(true)
                .setSelectFolderEnabled(false)
                .setMode(picker.DocsViewMode.LIST);
            // My Drive, then shared drives. One file at a time:
            // MULTISELECT_ENABLED is left off.
            built = new picker.PickerBuilder()
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
                        settle(() => resolve(typeof id === 'string' && id ? id : null));
                    } else if (action === picker.Action.CANCEL) {
                        settle(() => resolve(null));
                    } else if (action === (picker.Action.ERROR ?? 'error')) {
                        settle(() => reject(new DriveError(PICKER_FAILED)));
                    }
                })
                .build();
            abandonOpenPicker = abandon;
            built.setVisible(true);
        } catch {
            settle(() => reject(new DriveError(PICKER_FAILED)));
            return;
        }
        request.onShown?.();
    });
}

export interface DrivePermission {
    id?: string;
    type?: string;
    role?: string;
    domain?: string;
    allowFileDiscovery?: boolean;
}

/** What someone who opens the file may do with it. */
export type DriveAccess = 'view' | 'comment' | 'edit';

const ACCESS_ORDER: DriveAccess[] = ['view', 'comment', 'edit'];

function accessOf(role: string | undefined): DriveAccess {
    if (role === 'commenter') return 'comment';
    if (role === 'writer' || role === 'fileOrganizer' || role === 'organizer' || role === 'owner') return 'edit';
    return 'view';
}

/** Who can open a Drive file with its link, as far as Google told us. */
export type DriveSharing =
    | { kind: 'anyone'; access: DriveAccess }
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
    const anyone = permissions.filter(p => p.type === 'anyone');
    if (anyone.length > 0) {
        // The most that "anyone" may do, when several permissions say so.
        const most = Math.max(...anyone.map(p => ACCESS_ORDER.indexOf(accessOf(p.role))));
        return { kind: 'anyone', access: ACCESS_ORDER[most] };
    }
    const domain = permissions.find(p => p.type === 'domain');
    if (domain) return { kind: 'domain', domain: domain.domain ?? '' };
    return { kind: 'restricted' };
}

/** The file's sharing in plain words. */
export function describeSharing(sharing: DriveSharing): string {
    switch (sharing.kind) {
        case 'anyone': return `Anyone with the link can ${sharing.access}`;
        case 'domain': return `Only people at ${sharing.domain || 'your organization'} with the link`;
        case 'restricted': return 'Restricted: only people it is shared with';
        case 'unknown': return 'Sharing could not be read for this file';
    }
}

/**
 * What `POST /links` accepts for a Drive file. The backend twin is
 * `DriveFileRef::checked` and `is_drive_url` in `backend/src/utils/drive.rs`,
 * and `MAX_TITLE_CHARS` in `backend/src/handlers/links.rs`: change them
 * together. The browser applies the same rules BEFORE it asks Google to
 * change a file's sharing, so a file is never made public for a link our own
 * server would then refuse.
 */
export const DRIVE_LINK_LIMITS = {
    /** Bytes, of `[A-Za-z0-9_-]` only. */
    fileIdLength: 200,
    /** Characters (code points), none of them a control character. */
    nameChars: 500,
    /** Bytes of UTF-8, none of them a control character. */
    mimeTypeBytes: 255,
    /** Characters (code points) of a link's title. */
    titleChars: 500,
} as const;

const DRIVE_LINK_HOSTS = ['docs.google.com', 'drive.google.com'];
// Unicode's Cc, which is what Rust's `char::is_control` tests.
const CONTROL_CHARACTERS = /\p{Cc}/gu;

/** `https` on docs.google.com or drive.google.com, on the default port. */
export function isDriveLinkUrl(url: string): boolean {
    // The browser's URL parser drops tabs and line breaks; our server refuses them.
    if (/\p{Cc}/u.test(url)) return false;
    let parsed: URL;
    try {
        parsed = new URL(url);
    } catch {
        return false;
    }
    return parsed.protocol === 'https:'
        && parsed.port === ''
        && parsed.username === ''
        && parsed.password === ''
        && DRIVE_LINK_HOSTS.includes(parsed.hostname);
}

/** The file's name as our server will store it: no control characters, 1 to 500 characters. */
export function cleanDriveName(name: string | undefined): string {
    const kept = Array.from((name ?? '').replace(CONTROL_CHARACTERS, '').trim())
        .slice(0, DRIVE_LINK_LIMITS.nameChars)
        .join('')
        .trim();
    return kept || 'Untitled';
}

/** The file's type as our server will store it: no control characters, at most 255 bytes. */
export function cleanDriveMimeType(mimeType: string | undefined): string {
    const encoder = new TextEncoder();
    let kept = '';
    let bytes = 0;
    for (const character of (mimeType ?? '').replace(CONTROL_CHARACTERS, '').trim()) {
        bytes += encoder.encode(character).length;
        if (bytes > DRIVE_LINK_LIMITS.mimeTypeBytes) break;
        kept += character;
    }
    return kept.trim();
}

export const UNSUPPORTED_DRIVE_FILE = 'This kind of Google file cannot be shared from here yet.';

/**
 * Why our server would refuse a link to this Drive file, in words for the
 * person, or `null` when it would not. The name and the type are not checked:
 * `readDriveFile` has already cleaned them so that they always pass.
 */
export function driveLinkProblem(file: { id: string; webViewLink: string }): string | null {
    if (!isDriveLinkUrl(file.webViewLink)) return UNSUPPORTED_DRIVE_FILE;
    if (file.id.length > DRIVE_LINK_LIMITS.fileIdLength || !/^[A-Za-z0-9_-]+$/.test(file.id)) {
        return 'This file cannot be shared from here: Google Drive gave it an id this app cannot store.';
    }
    return null;
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
        name: cleanDriveName(file.name),
        mimeType: cleanDriveMimeType(file.mimeType),
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
