import { useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';
import { CirclePlay, ExternalLink, Loader2 } from 'lucide-react';
import { API_ENDPOINTS, shortLinkBase } from '../config/api';
import SEO from '../components/SEO';
import logger from '../utils/logger';
import { embedHref, httpsHref, providerName } from '../utils/media';

/** What `POST /{code}/media` answers. */
interface Media {
    provider: string;
    media_id: string;
    title: string;
    cta: { label: string; url: string } | null;
    embed: { src: string };
}

type Stage =
    | { name: 'loading' }
    | { name: 'ready'; media: Media; src: string }
    /** No ticket, or one the server no longer takes: the link has to be opened again. */
    | { name: 'expired' }
    /** The link itself is over: expired, disabled, not started. */
    | { name: 'gone'; message: string }
    | { name: 'failed'; message: string };

/**
 * The ticket `GET /{code}` put in the address's fragment (`#t=...`). A
 * fragment is never sent to a server, so the ticket is in no access log.
 */
function ticketInAddress(): string {
    const match = /(?:^#|&)t=([^&]+)/.exec(window.location.hash);
    if (!match) return '';
    try {
        return decodeURIComponent(match[1]);
    } catch {
        return '';
    }
}

function sentence(reason: unknown, fallback: string): string {
    if (typeof reason !== 'string' || !reason.trim()) return fallback;
    return reason.endsWith('.') ? reason : `${reason}.`;
}

async function errorOf(response: Response): Promise<unknown> {
    try {
        return (await response.json())?.error;
    } catch {
        return undefined;
    }
}

const LINK_OVER = 'This link has expired or is no longer available.';
const CANNOT_SHOW = 'This video cannot be shown.';

/**
 * A media link's page: `/m/{code}`. A visitor lands here from the short link
 * with a ticket, and is shown the link's title, the video in its host's own
 * player, and the sharer's button when there is one.
 *
 * The ticket stays in the address, unlike the Secure View viewer's: it opens
 * nothing but this page, counts nothing, and keeping it is what makes a
 * reload work. The player's address comes from the server and is still
 * checked here against the three hosts a media page may frame.
 */
export default function MediaPage() {
    const { code = '' } = useParams<{ code: string }>();
    const [ticket] = useState(ticketInAddress);
    const [stage, setStage] = useState<Stage>(ticket ? { name: 'loading' } : { name: 'expired' });

    useEffect(() => {
        if (!ticket) return;
        let ignore = false;
        (async () => {
            try {
                const response = await fetch(API_ENDPOINTS.mediaOpen(code), {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ ticket }),
                });
                if (ignore) return;
                if (response.status === 401) {
                    setStage({ name: 'expired' });
                } else if (response.status === 410) {
                    setStage({ name: 'gone', message: sentence(await errorOf(response), LINK_OVER) });
                } else if (response.status === 429) {
                    setStage({ name: 'failed', message: 'Too many requests. Try again in a minute.' });
                } else if (!response.ok) {
                    setStage({ name: 'failed', message: 'The video could not be loaded. Try again in a moment.' });
                } else {
                    const media = await response.json() as Media;
                    if (ignore) return;
                    // Whatever answered, only one of the three players is
                    // framed, at the address as the URL parser reads it.
                    const src = embedHref(media?.embed?.src);
                    setStage(src
                        ? { name: 'ready', media, src }
                        : { name: 'failed', message: CANNOT_SHOW });
                }
            } catch (error) {
                if (ignore) return;
                logger.error('Failed to load a media page', error);
                setStage({ name: 'failed', message: 'Network error. Check your connection and try again.' });
            }
        })();
        return () => { ignore = true; };
    }, [code, ticket]);

    const again = `${shortLinkBase()}/${encodeURIComponent(code)}`;

    if (stage.name === 'ready') {
        const { media, src } = stage;
        const title = typeof media.title === 'string' ? media.title.trim() : '';
        const provider = providerName(media.provider);
        // The parsed, absolute address: a link stored as `https:host` would
        // otherwise open a path on this site.
        const ctaUrl = media.cta && typeof media.cta.label === 'string' && media.cta.label.trim()
            ? httpsHref(media.cta.url)
            : null;
        const cta = media.cta && ctaUrl ? { label: media.cta.label.trim(), url: ctaUrl } : null;
        return (
            <div className="mx-auto w-full max-w-4xl px-4 py-6 sm:py-10">
                <SEO title={title || 'Video'} description="A shared video." noIndex />
                {title && (
                    <h1 className="mb-4 font-display text-xl font-bold tracking-tight text-ink break-words sm:text-2xl">
                        {title}
                    </h1>
                )}
                {/* 16:9 at any width: the frame fills a box that keeps the ratio. */}
                <div className="relative aspect-video w-full overflow-hidden rounded-xl bg-black shadow-subtle">
                    <iframe
                        src={src}
                        title={title || `${provider} video`}
                        allow="fullscreen; picture-in-picture; encrypted-media"
                        allowFullScreen
                        // YouTube's player asks for the embedding site's origin
                        // and refuses to play when it gets no Referer at all.
                        referrerPolicy="strict-origin-when-cross-origin"
                        className="absolute inset-0 h-full w-full border-0"
                    />
                </div>
                {cta && (
                    <div className="mt-6 text-center">
                        <a
                            href={cta.url}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="inline-flex max-w-full items-center justify-center gap-2 rounded-lg bg-primary-600 px-6 py-3 text-base font-semibold text-white transition-colors hover:bg-primary-700"
                        >
                            <span className="break-words">{cta.label}</span>
                            <ExternalLink className="h-4 w-4 shrink-0" aria-hidden="true" />
                        </a>
                    </div>
                )}
            </div>
        );
    }

    return (
        <div className="flex min-h-[70vh] items-center justify-center px-4 py-10">
            <SEO title="Video" description="A shared video." noIndex />
            <div className="w-full max-w-md rounded-2xl border border-line2 bg-surface p-8 shadow-subtle">
                <div className="mb-5 flex h-12 w-12 items-center justify-center rounded-xl bg-primary-50">
                    <CirclePlay className="h-6 w-6 text-primary-600" aria-hidden="true" />
                </div>

                {stage.name === 'loading' && (
                    <div role="status" className="flex items-center gap-3 text-sm text-muted">
                        <Loader2 className="h-5 w-5 animate-spin text-primary-600" aria-hidden="true" />
                        Loading the video…
                    </div>
                )}

                {stage.name === 'expired' && (
                    <div>
                        <h1 className="font-display text-xl font-bold tracking-tight text-ink">
                            This page has expired
                        </h1>
                        <p className="mt-2 text-sm text-muted">
                            Open the link again to watch the video.
                        </p>
                        <a
                            href={again}
                            className="mt-5 inline-block rounded-lg bg-primary-600 px-4 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-primary-700"
                        >
                            Open the link again
                        </a>
                    </div>
                )}

                {stage.name === 'gone' && (
                    <div>
                        <h1 className="font-display text-xl font-bold tracking-tight text-ink">
                            This video is not available
                        </h1>
                        <p role="alert" className="mt-2 text-sm text-muted">{stage.message}</p>
                    </div>
                )}

                {stage.name === 'failed' && (
                    <div>
                        <h1 className="font-display text-xl font-bold tracking-tight text-ink">
                            Something went wrong
                        </h1>
                        <p role="alert" className="mt-2 text-sm text-muted">{stage.message}</p>
                        <a
                            href={again}
                            className="mt-5 inline-block rounded-lg bg-primary-600 px-4 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-primary-700"
                        >
                            Open the link again
                        </a>
                    </div>
                )}
            </div>
        </div>
    );
}
