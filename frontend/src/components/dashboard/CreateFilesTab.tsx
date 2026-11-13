import { useEffect, useRef, useState, type DragEvent, type FormEvent } from 'react';
import { FileText, Upload, X } from 'lucide-react';
import {
    API_ENDPOINTS,
    authFetch,
    failureMessage,
    responseError,
    shortLinkBase,
    type FileUsage,
    type StartUploadResponse,
} from '../../config/api';
import logger from '../../utils/logger';
import { formatBytes } from '../../utils/fileSize';
import { pluralize } from '../../utils/plural';
import { putFile, UploadCancelled } from '../../utils/uploadFile';
import AliasField from './AliasField';
import { aliasProblem } from './aliasCheck';
import type { AppSettings } from './types';

interface CreateFilesTabProps {
    appSettings: AppSettings;
    /** The tab is the one showing. Storage use is only asked for then. */
    active: boolean;
    /** A file link was made: reload the link list. */
    onCreated: () => void;
    /** Set while a team's links are open. The new link is created in that team. */
    orgId?: number | null;
    orgName?: string | null;
}

type Phase = 'idle' | 'uploading' | 'finishing';

const labelClass = 'block font-mono text-xs uppercase tracking-[0.14em] text-faint mb-1.5';
const fieldClass = 'w-full rounded-lg border border-line2 bg-surface px-4 py-2 text-sm text-ink outline-none transition-colors focus:border-primary-500 placeholder:text-faint';
const providerClass = 'flex w-full cursor-not-allowed items-center gap-3 rounded-lg border border-line2 bg-paper px-4 py-3 text-left text-sm font-semibold text-muted opacity-70';

// Drawn here, not loaded: nothing on this tab talks to Google or Microsoft.
function DriveMark() {
    return (
        <svg viewBox="0 0 24 22" width="22" height="20" aria-hidden="true">
            <path d="M8 1h8l7.5 13h-8z" fill="#FBBC04" />
            <path d="M8 1 .5 14l4 7L12 8z" fill="#34A853" />
            <path d="M8 14h15.5l-4 7h-15z" fill="#4285F4" />
        </svg>
    );
}

function OneDriveMark() {
    return (
        <svg viewBox="0 0 24 16" width="24" height="16" aria-hidden="true">
            <path d="M9.6 4.3a5 5 0 0 1 8.6 1.9 4.2 4.2 0 0 1 1 8.3H7a4.6 4.6 0 0 1-.7-9.1 5 5 0 0 1 3.3-1.1z" fill="#0A64C8" />
            <path d="M14.4 7.2a4.6 4.6 0 0 1 3.8-1 4.2 4.2 0 0 1 1 8.3H7.6z" fill="#28A8EA" />
        </svg>
    );
}

/** Files tab of the create card: upload a file and share it as a short link. */
export default function CreateFilesTab({ appSettings, active, onCreated, orgId = null, orgName = null }: CreateFilesTabProps) {
    const enabled = appSettings.files?.enabled === true;
    const maxBytes = appSettings.files?.max_bytes ?? 0;

    const [file, setFile] = useState<File | null>(null);
    const [alias, setAlias] = useState('');
    const [title, setTitle] = useState('');
    const [titleTouched, setTitleTouched] = useState(false);
    const [expiresAt, setExpiresAt] = useState('');
    const [phase, setPhase] = useState<Phase>('idle');
    const [progress, setProgress] = useState(0);
    const [error, setError] = useState('');
    const [sharedUrl, setSharedUrl] = useState('');
    const [dragging, setDragging] = useState(false);
    const [usage, setUsage] = useState<FileUsage | null>(null);
    const [usageAttempt, setUsageAttempt] = useState(0);
    const upload = useRef<AbortController | null>(null);
    const picker = useRef<HTMLInputElement | null>(null);

    // Storage use is this account's own, so it is asked for only once the tab
    // is opened, and again after each upload.
    useEffect(() => {
        if (!enabled || !active) return;
        let ignore = false;
        (async () => {
            try {
                const res = await authFetch(API_ENDPOINTS.fileUsage);
                if (!res.ok || ignore) return;
                const data = await res.json() as FileUsage;
                if (!ignore) setUsage(data);
            } catch (err) {
                logger.error('Failed to load storage use', err);
            }
        })();
        return () => { ignore = true; };
    }, [enabled, active, usageAttempt]);

    // Leaving the page stops an upload in flight.
    useEffect(() => () => upload.current?.abort(), []);

    if (!enabled) {
        return (
            <div className="py-6 text-sm text-muted">
                <p className="font-semibold text-ink">File sharing is not set up on this server.</p>
            </div>
        );
    }

    const busy = phase !== 'idle';
    const tooLarge = file !== null && file.size > maxBytes;
    const empty = file !== null && file.size === 0;
    const fileProblem = tooLarge
        ? `This file is too large. The largest file you can share is ${formatBytes(maxBytes)}.`
        : empty
            ? 'This file is empty.'
            : '';

    const choose = (chosen: File | null) => {
        if (busy) return;
        setFile(chosen);
        setError('');
        setSharedUrl('');
        setProgress(0);
        if (chosen && !titleTouched) setTitle(chosen.name);
        if (!chosen && !titleTouched) setTitle('');
    };

    const onDrop = (event: DragEvent<HTMLLabelElement>) => {
        event.preventDefault();
        setDragging(false);
        choose(event.dataTransfer.files?.[0] ?? null);
    };

    const cancel = () => upload.current?.abort();

    const handleCreate = async (event: FormEvent) => {
        event.preventDefault();
        if (!file || fileProblem || busy) return;
        setError('');
        setSharedUrl('');

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

        const controller = new AbortController();
        upload.current = controller;
        setPhase('uploading');
        setProgress(0);
        try {
            const started = await authFetch(API_ENDPOINTS.fileUploads, {
                method: 'POST',
                body: JSON.stringify({ filename: file.name, content_type: file.type, size_bytes: file.size }),
                signal: controller.signal,
            });
            if (!started.ok) {
                setError(await responseError(started, 'Could not start the upload'));
                return;
            }
            const target = await started.json() as StartUploadResponse;

            await putFile(target.upload_url, target.required_headers, file, setProgress, controller.signal);

            setPhase('finishing');
            const body: Record<string, unknown> = {};
            if (alias) body.custom_alias = alias;
            if (title.trim()) body.title = title.trim();
            if (expiration) body.expires_at = expiration;
            if (orgId != null) body.org_id = orgId;
            const completed = await authFetch(API_ENDPOINTS.fileUploadComplete(target.upload_id), {
                method: 'POST',
                body: JSON.stringify(body),
            });
            if (!completed.ok) {
                setError(await responseError(completed, 'The file was uploaded, but the link could not be created'));
                return;
            }
            const link = await completed.json() as { code: string };
            setSharedUrl(`${shortLinkBase()}/${link.code}`);
            setFile(null);
            setAlias('');
            setTitle('');
            setTitleTouched(false);
            setExpiresAt('');
            if (picker.current) picker.current.value = '';
            setUsageAttempt(n => n + 1);
            onCreated();
        } catch (err) {
            if (err instanceof UploadCancelled || controller.signal.aborted) {
                setError('');
            } else {
                logger.error('Failed to share a file', err);
                setError(failureMessage(err, 'The upload failed. Check your connection and try again.'));
            }
        } finally {
            upload.current = null;
            setPhase('idle');
        }
    };

    const percent = Math.round(progress * 100);

    return (
        <div>
            {orgId != null && orgName && (
                <p className="mb-4 text-sm text-muted">Creating in {orgName}</p>
            )}
            <div className="space-y-2">
                <button type="button" disabled className={providerClass}>
                    <DriveMark />
                    <span className="flex-1">Google Drive Files</span>
                    <span className="text-xs font-normal text-faint">Coming soon</span>
                </button>
                <button type="button" disabled className={providerClass}>
                    <OneDriveMark />
                    <span className="flex-1">Microsoft OneDrive Files</span>
                    <span className="text-xs font-normal text-faint">Coming soon</span>
                </button>
            </div>

            <form onSubmit={handleCreate} className="mt-5 border-t border-line2 pt-5">
                <h3 className="mb-3 font-display text-base font-bold text-ink">Upload a File</h3>

                <label
                    htmlFor="file-upload-input"
                    onDragOver={(e) => { e.preventDefault(); if (!busy) setDragging(true); }}
                    onDragLeave={() => setDragging(false)}
                    onDrop={onDrop}
                    className={`flex cursor-pointer flex-col items-center gap-2 rounded-lg border-2 border-dashed px-4 py-6 text-center text-sm transition-colors ${
                        dragging ? 'border-primary-500 bg-primary-50/50' : 'border-line2 bg-paper hover:border-primary-500'
                    } ${busy ? 'pointer-events-none opacity-60' : ''}`}
                >
                    <Upload className="h-5 w-5 text-faint" aria-hidden="true" />
                    <span className="font-semibold text-ink">Drop a file here, or choose one</span>
                    <span className="text-xs text-faint">Any type, up to {formatBytes(maxBytes)}</span>
                    <input
                        ref={picker}
                        id="file-upload-input"
                        type="file"
                        className="sr-only"
                        aria-label="File to share"
                        disabled={busy}
                        onChange={(e) => choose(e.target.files?.[0] ?? null)}
                    />
                </label>

                {file && (
                    <div className="mt-3 flex items-center gap-2 rounded-lg border border-line2 bg-surface px-3 py-2 text-sm">
                        <FileText className="h-4 w-4 shrink-0 text-primary-600" aria-hidden="true" />
                        <span className="min-w-0 flex-1 truncate font-medium text-ink" title={file.name}>{file.name}</span>
                        <span className="shrink-0 text-xs text-muted">{formatBytes(file.size)}</span>
                        {!busy && (
                            <button
                                type="button"
                                onClick={() => { choose(null); if (picker.current) picker.current.value = ''; }}
                                aria-label="Remove file"
                                className="rounded p-1 text-faint transition-colors hover:bg-line hover:text-ink"
                            >
                                <X className="h-4 w-4" />
                            </button>
                        )}
                    </div>
                )}
                {fileProblem && (
                    <p role="alert" className="mt-2 text-sm text-danger">{fileProblem}</p>
                )}

                {file && !fileProblem && (
                    <div className="mt-4 space-y-4">
                        {appSettings.custom_aliases_enabled && (
                            <div>
                                <AliasField
                                    idPrefix="file"
                                    alias={alias}
                                    onChange={setAlias}
                                    settings={appSettings}
                                    labelClass={labelClass}
                                    disabled={busy}
                                />
                            </div>
                        )}
                        <div>
                            <label htmlFor="file-title" className={labelClass}>Title</label>
                            <input
                                id="file-title"
                                type="text"
                                className={fieldClass}
                                value={title}
                                maxLength={500}
                                disabled={busy}
                                onChange={(e) => { setTitle(e.target.value); setTitleTouched(true); }}
                            />
                        </div>
                        <div>
                            <label htmlFor="file-expires" className={labelClass}>Expires (optional)</label>
                            <input
                                id="file-expires"
                                type="date"
                                className={fieldClass}
                                value={expiresAt}
                                disabled={busy}
                                aria-describedby="file-expires-help"
                                onChange={(e) => setExpiresAt(e.target.value)}
                            />
                            <p id="file-expires-help" className="mt-1.5 text-xs text-faint">
                                Leave empty and the link never expires. When a link expires or is deleted, its file is deleted too.
                            </p>
                        </div>
                    </div>
                )}

                {busy && (
                    <div className="mt-4">
                        <div
                            role="progressbar"
                            aria-label="Upload progress"
                            aria-valuemin={0}
                            aria-valuemax={100}
                            aria-valuenow={percent}
                            className="h-2 w-full overflow-hidden rounded-full bg-line"
                        >
                            <div className="h-full bg-primary-600 transition-[width]" style={{ width: `${percent}%` }} />
                        </div>
                        <p className="mt-1.5 text-xs text-muted">
                            {phase === 'finishing' ? 'Creating the link…' : `Uploading… ${percent}%`}
                        </p>
                    </div>
                )}

                {error && (
                    <p role="alert" className="mt-3 text-sm text-danger">{error}</p>
                )}
                {sharedUrl && (
                    <p role="status" className="mt-3 text-sm text-success">
                        File shared: <span className="font-mono font-semibold">{sharedUrl.replace(/^https?:\/\//, '')}</span>
                    </p>
                )}

                <div className="mt-4 flex items-center gap-3">
                    <button
                        type="submit"
                        disabled={!file || Boolean(fileProblem) || busy}
                        className="rounded-lg bg-primary-600 px-5 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-primary-700 disabled:cursor-not-allowed disabled:opacity-50"
                    >
                        {busy ? 'Uploading…' : 'Create'}
                    </button>
                    {phase === 'uploading' && (
                        <button
                            type="button"
                            onClick={cancel}
                            className="rounded-lg border border-line2 px-4 py-2.5 text-sm font-semibold text-muted transition-colors hover:text-ink"
                        >
                            Cancel
                        </button>
                    )}
                </div>
            </form>

            {usage && (
                <p className="mt-5 border-t border-line2 pt-4 text-xs text-muted">
                    Your uploads: {formatBytes(usage.bytes_used)} in {usage.file_count.toLocaleString()} {pluralize(usage.file_count, 'file')}
                </p>
            )}
        </div>
    );
}
