import { useRef, useState, type FormEvent } from 'react';
import { API_ENDPOINTS, authFetch, failureMessage, responseError, shortLinkBase } from '../../config/api';
import logger from '../../utils/logger';
import { localIsoDate } from '../../utils/localIsoDate';
import { ctaFields, ctaProblem, parseMediaUrl, PROVIDER_NAMES } from '../../utils/media';
import AliasField from './AliasField';
import { aliasProblem } from './aliasCheck';
import DriveFilePanel from './DriveFilePanel';
import MediaCtaFields from './MediaCtaFields';
import type { AppSettings, LinkData } from './types';
import type { DriveEnv } from '../../utils/googleDrive';

interface CreateMediaTabProps {
    appSettings: AppSettings;
    /** A media link was made: reload the link list. */
    onCreated: () => void;
    /** Set while a team's links are open. The new link is created in that team. */
    orgId?: number | null;
    orgName?: string | null;
    /** Google, as the Drive button reaches it. Tests pass a stand-in. */
    driveEnv?: DriveEnv;
}

const labelClass = 'block font-mono text-xs uppercase tracking-[0.14em] text-faint mb-1.5';
const fieldClass = 'w-full rounded-lg border border-line2 bg-surface px-4 py-2 text-sm text-ink outline-none transition-colors focus:border-primary-500 placeholder:text-faint';

const TITLE_CHARS = 500;
const NOT_CHECKED = 'We could not check that this video can be embedded. Open the link once to make sure it plays.';

/**
 * Media tab of the create card: turn a YouTube or Vimeo address, or a video
 * picked in Google Drive, into a short link that opens a page with the player
 * on it. The server reads the video out of the address and refuses one that
 * cannot be embedded; the hint here only says what the address looks like.
 */
export default function CreateMediaTab({ appSettings, onCreated, orgId = null, orgName = null, driveEnv }: CreateMediaTabProps) {
    const [url, setUrl] = useState('');
    const [title, setTitle] = useState('');
    const [ctaLabel, setCtaLabel] = useState('');
    const [ctaUrl, setCtaUrl] = useState('');
    const [alias, setAlias] = useState('');
    const [expiresAt, setExpiresAt] = useState('');
    const [creating, setCreating] = useState(false);
    const [error, setError] = useState('');
    const [shared, setShared] = useState<{ url: string; note: string } | null>(null);
    const inFlight = useRef(false);

    const typed = url.trim();
    const recognised = typed ? parseMediaUrl(url) : null;

    const handleCreate = async (event: FormEvent) => {
        event.preventDefault();
        if (inFlight.current) return;
        setError('');
        setShared(null);

        if (!recognised) {
            setError('Paste the address of one YouTube or Vimeo video.');
            return;
        }
        const badButton = ctaProblem(ctaLabel, ctaUrl);
        if (badButton) {
            setError(badButton);
            return;
        }
        const badAlias = aliasProblem(alias, appSettings);
        if (badAlias) {
            setError(badAlias);
            return;
        }
        if (Array.from(title.trim()).length > TITLE_CHARS) {
            setError(`Title must be at most ${TITLE_CHARS} characters`);
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

        inFlight.current = true;
        setCreating(true);
        try {
            // The address goes as it was typed: the server reads the video
            // out of it. No player address is ever sent.
            const body: Record<string, unknown> = {
                original_url: typed,
                media: ctaFields(ctaLabel, ctaUrl),
            };
            if (alias) body.custom_alias = alias;
            if (title.trim()) body.title = title.trim();
            if (expiration) body.expires_at = expiration;
            if (orgId != null) body.org_id = orgId;
            const res = await authFetch(API_ENDPOINTS.links, { method: 'POST', body: JSON.stringify(body) });
            if (!res.ok) {
                setError(await responseError(res, 'The link could not be created'));
                return;
            }
            const link = await res.json() as Pick<LinkData, 'code' | 'media'>;
            setShared({
                url: `${shortLinkBase()}/${link.code}`,
                note: link.media?.embed_check === 'not_checked' ? NOT_CHECKED : '',
            });
            setUrl('');
            setTitle('');
            setCtaLabel('');
            setCtaUrl('');
            setAlias('');
            setExpiresAt('');
            onCreated();
        } catch (err) {
            logger.error('Failed to share a video', err);
            setError(failureMessage(err, 'The link could not be created. Check your connection and try again.'));
        } finally {
            inFlight.current = false;
            setCreating(false);
        }
    };

    return (
        <div>
            {orgId != null && orgName && (
                <p className="mb-4 text-sm text-muted">Sharing in {orgName}</p>
            )}
            <p className="mb-4 text-sm text-muted">
                Share a video on a page of its own, with the player on it. Opening the short link shows that page.
            </p>

            <form onSubmit={handleCreate} aria-label="Share a YouTube or Vimeo video" className="space-y-4">
                <div>
                    <label htmlFor="media-url" className={labelClass}>YouTube or Vimeo address</label>
                    <input
                        id="media-url"
                        type="url"
                        inputMode="url"
                        className={fieldClass}
                        placeholder="https://www.youtube.com/watch?v=…"
                        value={url}
                        maxLength={2048}
                        disabled={creating}
                        aria-describedby="media-url-hint"
                        onChange={(e) => { setUrl(e.target.value); setError(''); }}
                    />
                    <p
                        id="media-url-hint"
                        role="status"
                        className={`mt-1.5 text-xs ${!typed ? 'text-faint' : recognised ? 'text-success' : 'text-danger'}`}
                    >
                        {!typed
                            ? 'Paste the address of one video.'
                            : recognised
                                ? `Recognised as a ${PROVIDER_NAMES[recognised.provider]} video.`
                                : 'Not recognised. Paste the address of one YouTube or Vimeo video.'}
                    </p>
                </div>

                {recognised && (
                    <>
                        <div>
                            <label htmlFor="media-title" className={labelClass}>Title</label>
                            <input
                                id="media-title"
                                type="text"
                                className={fieldClass}
                                placeholder="Leave empty to use the video's own title"
                                value={title}
                                maxLength={TITLE_CHARS}
                                disabled={creating}
                                onChange={(e) => setTitle(e.target.value)}
                            />
                        </div>
                        <MediaCtaFields
                            idPrefix="media"
                            label={ctaLabel}
                            url={ctaUrl}
                            onLabelChange={setCtaLabel}
                            onUrlChange={setCtaUrl}
                            labelClass={labelClass}
                            fieldClass={fieldClass}
                            disabled={creating}
                        />
                        {appSettings.custom_aliases_enabled && (
                            <div>
                                <AliasField
                                    idPrefix="media"
                                    alias={alias}
                                    onChange={setAlias}
                                    settings={appSettings}
                                    labelClass={labelClass}
                                    disabled={creating}
                                />
                            </div>
                        )}
                        <div>
                            <label htmlFor="media-expires" className={labelClass}>Expires (optional)</label>
                            <input
                                id="media-expires"
                                type="date"
                                className={fieldClass}
                                value={expiresAt}
                                disabled={creating}
                                aria-describedby="media-expires-help"
                                onChange={(e) => setExpiresAt(e.target.value)}
                                min={localIsoDate()}
                            />
                            <p id="media-expires-help" className="mt-1.5 text-xs text-faint">
                                Leave empty and the link never expires. The video stays where it is either way.
                            </p>
                        </div>
                    </>
                )}

                {error && (
                    <p role="alert" className="text-sm text-danger">{error}</p>
                )}
                {shared && (
                    <div className="text-sm">
                        <p role="status" className="text-success">
                            Video shared: <span className="font-mono font-semibold">{shared.url.replace(/^https?:\/\//, '')}</span>
                        </p>
                        {shared.note && (
                            <p role="alert" className="mt-1 text-ink">{shared.note}</p>
                        )}
                    </div>
                )}

                <button
                    type="submit"
                    disabled={!recognised || creating}
                    className="rounded-lg bg-primary-600 px-5 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-primary-700 disabled:cursor-not-allowed disabled:opacity-50"
                >
                    {creating ? 'Creating…' : 'Create'}
                </button>
            </form>

            <div className="mt-5 border-t border-line2 pt-5">
                <h3 className="mb-3 font-display text-base font-bold text-ink">Or a video in Google Drive</h3>
                <DriveFilePanel
                    appSettings={appSettings}
                    onCreated={onCreated}
                    orgId={orgId}
                    media
                    {...(driveEnv ? { driveEnv } : {})}
                />
            </div>
        </div>
    );
}
