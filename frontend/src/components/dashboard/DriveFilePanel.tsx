import { useRef, useState, type FormEvent } from 'react';
import { File, FileSpreadsheet, FileText, Film, Image, Music, Presentation, X, type LucideIcon } from 'lucide-react';
import { API_ENDPOINTS, authFetch, failureMessage, responseError, shortLinkBase, type CheckCodeResponse } from '../../config/api';
import logger from '../../utils/logger';
import {
    browserDriveEnv,
    describeSharing,
    driveAccessToken,
    DriveError,
    DRIVE_LINK_LIMITS,
    driveLinkProblem,
    emailFromToken,
    pickDriveFile,
    readDriveFile,
    shareWithAnyone,
    type DriveEnv,
    type DriveFile,
} from '../../utils/googleDrive';
import { localIsoDate } from '../../utils/localIsoDate';
import { ctaFields, ctaProblem, isVideoMime } from '../../utils/media';
import { safeLocalStorage } from '../../utils/storage';
import AliasField from './AliasField';
import { aliasProblem } from './aliasCheck';
import DriveMark from './DriveMark';
import MediaCtaFields from './MediaCtaFields';
import type { AppSettings } from './types';

interface DriveFilePanelProps {
    appSettings: AppSettings;
    /** A link was made: reload the link list. */
    onCreated: () => void;
    /** Set while a team's links are open. The new link is created in that team. */
    orgId?: number | null;
    /** Google, as the panel reaches it. Tests pass a stand-in. */
    driveEnv?: DriveEnv;
    /**
     * The Media tab's "Google Drive video": only a video can be picked, and
     * the link opens a page with Google's player on it. Everything else is
     * this same panel: the sharing is read, shown and changed in one place.
     */
    media?: boolean;
}

const labelClass = 'block font-mono text-xs uppercase tracking-[0.14em] text-faint mb-1.5';
const fieldClass = 'w-full rounded-lg border border-line2 bg-surface px-4 py-2 text-sm text-ink outline-none transition-colors focus:border-primary-500 placeholder:text-faint';
const providerBase = 'flex w-full items-center gap-3 rounded-lg border border-line2 px-4 py-3 text-left text-sm font-semibold';

/** A local icon for the file's type: Google's own icons are never loaded. */
function typeIcon(mimeType: string): LucideIcon {
    if (mimeType.includes('spreadsheet') || mimeType.includes('excel') || mimeType === 'text/csv') return FileSpreadsheet;
    if (mimeType.includes('presentation') || mimeType.includes('powerpoint')) return Presentation;
    if (mimeType.startsWith('image/')) return Image;
    if (mimeType.startsWith('video/')) return Film;
    if (mimeType.startsWith('audio/')) return Music;
    if (mimeType.includes('document') || mimeType === 'application/pdf' || mimeType.startsWith('text/')) return FileText;
    return File;
}

/** What the person is told when the link fails after the sharing was changed. */
const SHARING_WAS_CHANGED = 'The file\'s sharing in Google Drive was changed to "anyone with the link can view", but no link was created.';
const AFTER_CHANGE_ADVICE = 'You can try again here (the sharing will not be changed a second time) or change the sharing back in Google Drive.';

/** Why the link was not made, with what happened to the file's sharing when it was changed first. */
function linkFailure(sharingChanged: boolean, reason: string): string {
    if (!sharingChanged) return reason;
    const sentence = /[.!?]$/.test(reason) ? reason : `${reason}.`;
    return `${SHARING_WAS_CHANGED} Reason: ${sentence} ${AFTER_CHANGE_ADVICE}`;
}

/**
 * "Google Drive Files": pick one file in Google's picker and share it as a
 * short link to where it lives in Drive. The file's sharing is read and shown
 * before the link is made, and changed only when the person ticks the box and
 * clicks Create. Everything our own server would refuse is checked before
 * that change, and a link that still fails after it says the sharing changed.
 */
export default function DriveFilePanel({ appSettings, onCreated, orgId = null, driveEnv = browserDriveEnv, media = false }: DriveFilePanelProps) {
    const drive = appSettings.drive;
    const clientId = appSettings.google_client_id;
    const ready = Boolean(drive?.api_key && drive?.app_id && clientId);
    // The Files tab and the Media tab are both on the page: each panel's
    // fields need ids of their own.
    const idPrefix = media ? 'drive-video' : 'drive';
    const buttonName = media ? 'Google Drive video' : 'Google Drive Files';
    const [ctaLabel, setCtaLabel] = useState('');
    const [ctaUrl, setCtaUrl] = useState('');

    const [opening, setOpening] = useState(false);
    // Google's picker has been asked to show; what happens in it is not ours to see.
    const [pickerShown, setPickerShown] = useState(false);
    const [creating, setCreating] = useState(false);
    const [file, setFile] = useState<DriveFile | null>(null);
    const [makePublic, setMakePublic] = useState(false);
    // This panel changed the picked file's sharing and no link exists for it yet.
    const [sharingChanged, setSharingChanged] = useState(false);
    const [alias, setAlias] = useState('');
    const [title, setTitle] = useState('');
    const [expiresAt, setExpiresAt] = useState('');
    const [error, setError] = useState('');
    const [shared, setShared] = useState<{ url: string; warning: string } | null>(null);
    const inFlight = useRef(false);
    // Which click on "Google Drive Files" is the current one. A picker that
    // never answers is left behind by the next click, or by Create.
    const pickRun = useRef(0);

    const token = () => driveAccessToken(
        { clientId: clientId ?? '', loginHint: emailFromToken(safeLocalStorage.getItem('token')) },
        driveEnv,
    );

    const reset = () => {
        setFile(null);
        setMakePublic(false);
        setSharingChanged(false);
        setAlias('');
        setTitle('');
        setExpiresAt('');
        setCtaLabel('');
        setCtaUrl('');
    };

    const choose = async () => {
        if (!ready || !drive || inFlight.current) return;
        pickRun.current += 1;
        const run = pickRun.current;
        const current = () => run === pickRun.current;
        inFlight.current = true;
        setOpening(true);
        setPickerShown(false);
        setError('');
        setShared(null);
        try {
            const accessToken = await token();
            const fileId = await pickDriveFile({
                token: accessToken,
                apiKey: drive.api_key,
                appId: drive.app_id,
                videosOnly: media,
                // The picker is Google's from here, and it may never answer
                // (a frame that did not load says nothing). The button is
                // given back so the person is never stuck waiting for it.
                onShown: () => {
                    if (!current()) return;
                    inFlight.current = false;
                    setOpening(false);
                    setPickerShown(true);
                },
            }, driveEnv);
            // A newer click, or Create, has taken over: this answer is not wanted.
            if (!current()) return;
            setPickerShown(false);
            // Closing the picker without picking is not an error.
            if (!fileId) return;
            inFlight.current = true;
            setOpening(true);
            const picked = await readDriveFile(fileId, accessToken, driveEnv);
            reset();
            // A file our server would refuse a link for is never offered:
            // no tick, no Create, so its sharing cannot be changed for nothing.
            const problem = driveLinkProblem(picked);
            if (problem) {
                setError(`"${picked.name}" was not added. ${problem}`);
                return;
            }
            // The same for a file the media page could not play: our server
            // takes only a video as a Drive media link.
            if (media && !isVideoMime(picked.mimeType)) {
                setError(`"${picked.name}" was not added. Only a video can be shared on a media page.`);
                return;
            }
            setFile(picked);
            setTitle(picked.name);
        } catch (err) {
            if (!current()) return;
            setPickerShown(false);
            if (!(err instanceof DriveError)) logger.error('Failed to pick a Google Drive file', err);
            setError(err instanceof DriveError ? err.message : 'Google Drive could not be opened. Please try again.');
        } finally {
            if (current()) {
                inFlight.current = false;
                setOpening(false);
            }
        }
    };

    /** Why `alias` cannot be used, asked of our server; `null` when it is free. */
    const aliasRefusal = async (code: string): Promise<string | null> => {
        const res = await authFetch(`${API_ENDPOINTS.checkCode}?${new URLSearchParams({ code })}`);
        if (!res.ok) return responseError(res, 'The alias could not be checked');
        const answer = await res.json() as CheckCodeResponse;
        return answer.available ? null : (answer.message || 'This alias is already taken');
    };

    const locked = file !== null && file.sharing.kind !== 'anyone';

    const handleCreate = async (event: FormEvent) => {
        event.preventDefault();
        if (!file || inFlight.current) return;
        setError('');

        const badAlias = aliasProblem(alias, appSettings);
        if (badAlias) {
            setError(badAlias);
            return;
        }
        let expiration: string | undefined;
        if (expiresAt) {
            const parsed = new Date(`${expiresAt}T23:59:00`);
            if (Number.isNaN(parsed.getTime())) {
                setError('Enter a valid expiration');
                return;
            }
            expiration = parsed.toISOString();
        }
        if (Array.from(title.trim()).length > DRIVE_LINK_LIMITS.titleChars) {
            setError(`Title must be at most ${DRIVE_LINK_LIMITS.titleChars} characters`);
            return;
        }
        // A button our server would refuse is caught here, before the file's
        // sharing can be changed for a link that would then not be made.
        const badButton = media ? ctaProblem(ctaLabel, ctaUrl) : '';
        if (badButton) {
            setError(badButton);
            return;
        }

        // A picker still open somewhere is left behind.
        pickRun.current += 1;
        setPickerShown(false);
        inFlight.current = true;
        setCreating(true);
        let changed = sharingChanged;
        try {
            let current = file;
            // The one change this app ever makes in Drive: asked for by the
            // tick, sent by this click, and finished before any link exists.
            if (locked && makePublic && file.canShare) {
                // The last thing our server could refuse that can be known
                // beforehand: an alias someone else already has.
                if (alias) {
                    let refusal: string | null;
                    try {
                        refusal = await aliasRefusal(alias);
                    } catch (err) {
                        setError(failureMessage(err, 'The alias could not be checked, so the file\'s sharing was not changed and no link was created. Check your connection and try again.'));
                        return;
                    }
                    if (refusal) {
                        const sentence = /[.!?]$/.test(refusal) ? refusal : `${refusal}.`;
                        setError(`${sentence} The file's sharing was not changed and no link was created.`);
                        return;
                    }
                }
                try {
                    await shareWithAnyone(file.id, await token(), driveEnv);
                } catch (err) {
                    if (!(err instanceof DriveError)) logger.error('Failed to change Google Drive sharing', err);
                    setError(err instanceof DriveError
                        ? `${err.message} No link was created. Untick the box to create the link without changing the sharing.`
                        : 'The sharing could not be changed, so no link was created.');
                    return;
                }
                // Recorded at once: a retry must not ask Google again, and
                // a link that now fails has to say the sharing changed.
                current = { ...file, sharing: { kind: 'anyone', access: 'view' } };
                setFile(current);
                setMakePublic(false);
                changed = true;
                setSharingChanged(true);
            }

            const body: Record<string, unknown> = {
                original_url: current.webViewLink,
                drive_file: { file_id: current.id, name: current.name, mime_type: current.mimeType },
            };
            // A media link: the server makes the page's player address from
            // the file's id itself. None is sent.
            if (media) body.media = { provider: 'drive', ...ctaFields(ctaLabel, ctaUrl) };
            if (alias) body.custom_alias = alias;
            if (title.trim()) body.title = title.trim();
            if (expiration) body.expires_at = expiration;
            if (orgId != null) body.org_id = orgId;
            const res = await authFetch(API_ENDPOINTS.links, { method: 'POST', body: JSON.stringify(body) });
            if (!res.ok) {
                setError(linkFailure(changed, await responseError(res, 'The link could not be created')));
                return;
            }
            const link = await res.json() as { code: string };
            setShared({
                url: `${shortLinkBase()}/${link.code}`,
                warning: current.sharing.kind === 'anyone'
                    ? ''
                    : media
                        ? 'The video\'s sharing was not changed. It will not play for people it is not shared with when they open this link.'
                        : 'The file\'s sharing was not changed. People it is not shared with will be asked to request access when they open this link.',
            });
            reset();
            onCreated();
        } catch (err) {
            logger.error('Failed to share a Google Drive file', err);
            // On a 401 `authFetch` has already sent the browser to /login;
            // the message is set all the same, for as long as this page lasts.
            setError(linkFailure(changed, failureMessage(err, 'The link could not be created. Check your connection and try again.')));
        } finally {
            inFlight.current = false;
            setCreating(false);
        }
    };

    const TypeIcon = file ? typeIcon(file.mimeType) : File;
    const busy = opening || creating;

    return (
        <div>
            <button
                type="button"
                disabled={!ready || busy}
                onClick={choose}
                className={`${providerBase} ${ready
                    ? 'bg-surface text-ink transition-colors hover:border-primary-500 disabled:cursor-wait disabled:opacity-70'
                    : 'cursor-not-allowed bg-paper text-muted opacity-70'}`}
            >
                <DriveMark />
                <span className="flex-1">{buttonName}</span>
                {!ready && <span className="text-xs font-normal text-faint">Not set up on this server</span>}
                {opening && <span className="text-xs font-normal text-faint">Opening…</span>}
            </button>
            {pickerShown && (
                <p className="mt-1.5 text-xs text-faint">
                    Google's file picker is open. If it did not appear, click {buttonName} again.
                </p>
            )}

            {file && (
                <form onSubmit={handleCreate} aria-label={media ? 'Share a Google Drive video' : 'Share a Google Drive file'} className="mt-3 space-y-4 rounded-lg border border-line2 bg-paper p-4">
                    <div className="flex items-center gap-2 text-sm">
                        <TypeIcon className="h-4 w-4 shrink-0 text-primary-600" aria-hidden="true" />
                        <span className="min-w-0 flex-1 truncate font-medium text-ink" title={file.name}>{file.name}</span>
                        {!creating && (
                            <button
                                type="button"
                                onClick={() => { reset(); setError(''); }}
                                aria-label="Remove Google Drive file"
                                className="rounded p-1 text-faint transition-colors hover:bg-line hover:text-ink"
                            >
                                <X className="h-4 w-4" />
                            </button>
                        )}
                    </div>
                    <p className="text-sm text-muted">
                        Sharing in Google Drive: <span className="font-semibold text-ink">{describeSharing(file.sharing)}</span>
                    </p>
                    {sharingChanged && (
                        <p role="note" className="rounded-lg border border-warning/40 bg-warning/10 p-3 text-sm text-ink">
                            You changed this file's sharing to "anyone with the link can view" a moment ago. No link has been created for it yet.
                        </p>
                    )}

                    {locked && (
                        <div className="rounded-lg border border-warning/40 bg-warning/10 p-3 text-sm text-ink">
                            <p role="alert">
                                {media
                                    ? (file.sharing.kind === 'unknown'
                                        ? 'The video may not play for people outside.'
                                        : 'The video will not play for people outside.')
                                    : (file.sharing.kind === 'unknown'
                                        ? 'People outside may not be able to open this file.'
                                        : 'People outside will not be able to open this file.')}
                            </p>
                            <label className="mt-2 flex items-start gap-2">
                                <input
                                    type="checkbox"
                                    className="mt-0.5"
                                    checked={makePublic}
                                    disabled={!file.canShare || creating}
                                    onChange={(e) => { setMakePublic(e.target.checked); setError(''); }}
                                />
                                <span>Change to: anyone with the link can view</span>
                            </label>
                            {!file.canShare && (
                                <p className="mt-1.5 text-xs text-muted">
                                    You cannot change this file's sharing. Ask its owner, or create the link as it is.
                                </p>
                            )}
                        </div>
                    )}

                    {appSettings.custom_aliases_enabled && (
                        <div>
                            <AliasField
                                idPrefix={idPrefix}
                                alias={alias}
                                onChange={setAlias}
                                settings={appSettings}
                                labelClass={labelClass}
                                disabled={creating}
                            />
                        </div>
                    )}
                    <div>
                        <label htmlFor={`${idPrefix}-title`} className={labelClass}>Title</label>
                        <input
                            id={`${idPrefix}-title`}
                            type="text"
                            className={fieldClass}
                            value={title}
                            maxLength={500}
                            disabled={creating}
                            onChange={(e) => setTitle(e.target.value)}
                        />
                    </div>
                    {media && (
                        <MediaCtaFields
                            idPrefix={idPrefix}
                            label={ctaLabel}
                            url={ctaUrl}
                            onLabelChange={setCtaLabel}
                            onUrlChange={setCtaUrl}
                            labelClass={labelClass}
                            fieldClass={fieldClass}
                            disabled={creating}
                        />
                    )}
                    <div>
                        <label htmlFor={`${idPrefix}-expires`} className={labelClass}>Expires (optional)</label>
                        <input
                            id={`${idPrefix}-expires`}
                            type="date"
                            className={fieldClass}
                            value={expiresAt}
                            disabled={creating}
                            aria-describedby={`${idPrefix}-expires-help`}
                            onChange={(e) => setExpiresAt(e.target.value)}
                            min={localIsoDate()}
                        />
                        <p id={`${idPrefix}-expires-help`} className="mt-1.5 text-xs text-faint">
                            Leave empty and the link never expires. The file stays in Google Drive either way.
                        </p>
                    </div>

                    <button
                        type="submit"
                        disabled={creating}
                        className="rounded-lg bg-primary-600 px-5 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-primary-700 disabled:cursor-not-allowed disabled:opacity-50"
                    >
                        {creating ? 'Creating…' : locked && makePublic ? 'Change sharing and create' : 'Create link'}
                    </button>
                </form>
            )}

            {error && (
                <p role="alert" className="mt-3 text-sm text-danger">{error}</p>
            )}
            {shared && (
                <div className="mt-3 text-sm">
                    <p role="status" className="text-success">
                        {media ? 'Video shared' : 'File shared'}: <span className="font-mono font-semibold">{shared.url.replace(/^https?:\/\//, '')}</span>
                    </p>
                    {shared.warning && (
                        <p role="alert" className="mt-1 text-ink">{shared.warning}</p>
                    )}
                </div>
            )}
        </div>
    );
}
