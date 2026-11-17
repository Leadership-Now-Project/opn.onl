// Media links: a short link that opens a page with a video's player on it.
//
// Nothing here decides anything. Which video a link is for, whether it can be
// embedded and what the player's address is are all decided by the server
// (`backend/src/utils/media.rs`). This file draws the "recognised as" hint
// while someone types, catches the mistakes the server would refuse before a
// request is sent, and is the page's second check of the player's address.

export type MediaProvider = 'youtube' | 'vimeo' | 'drive';

export const PROVIDER_NAMES: Record<MediaProvider, string> = {
    youtube: 'YouTube',
    vimeo: 'Vimeo',
    drive: 'Google Drive',
};

/** The provider's name for display; an unknown one is shown as "Video". */
export function providerName(provider: string): string {
    return (PROVIDER_NAMES as Record<string, string>)[provider] ?? 'Video';
}

export interface ParsedMedia {
    provider: 'youtube' | 'vimeo';
    /** YouTube's 11-character id, or Vimeo's number (with `:hash` when unlisted). */
    mediaId: string;
}

const MAX_URL_LENGTH = 2048;
const YOUTUBE_ID = /^[A-Za-z0-9_-]{11}$/;
/** As long as an id, but a playlist player and a channel's live player. */
const YOUTUBE_NOT_IDS = ['videoseries', 'live_stream'];
const VIMEO_ID = /^[1-9][0-9]{0,11}$/;
const VIMEO_HASH = /^[0-9a-z]{6,32}$/;
/**
 * Words Vimeo puts after a video's or a member's number that have the shape
 * of an unlisted video's hash and are pages, not videos. Twin of
 * `VIMEO_NOT_HASHES` in `backend/src/utils/media.rs`.
 */
const VIMEO_NOT_HASHES = [
    'settings', 'likes', 'collections', 'videos', 'following', 'followers', 'channels',
    'groups', 'albums', 'portfolios', 'appearances', 'members', 'comments', 'review',
];

/** The one value of a query parameter; `undefined` when it is given twice. */
function oneParam(url: URL, name: string): string | null | undefined {
    const values = url.searchParams.getAll(name);
    if (values.length > 1) return undefined;
    return values.length === 1 ? values[0] : null;
}

function youtube(id: string | null | undefined): ParsedMedia | null {
    if (!id || !YOUTUBE_ID.test(id) || YOUTUBE_NOT_IDS.includes(id)) return null;
    return { provider: 'youtube', mediaId: id };
}

function vimeo(id: string, hash: string | null | undefined): ParsedMedia | null {
    if (hash === undefined || !VIMEO_ID.test(id)) return null;
    if (hash === null) return { provider: 'vimeo', mediaId: id };
    return VIMEO_HASH.test(hash) && !VIMEO_NOT_HASHES.includes(hash)
        ? { provider: 'vimeo', mediaId: `${id}:${hash}` }
        : null;
}

/**
 * Read a YouTube or Vimeo video out of an address, for the "recognised as"
 * hint. Twin of `parse_media_url` in `backend/src/utils/media.rs`, which is
 * the one that decides: change the two together. `null` for anything that is
 * not exactly one of the forms these hosts hand out.
 */
export function parseMediaUrl(raw: string): ParsedMedia | null {
    // The browser's URL parser drops tabs and line breaks; the server refuses them.
    if (/\p{Cc}/u.test(raw)) return null;
    const text = raw.replace(/^ +| +$/g, '');
    if (!text || text.length > MAX_URL_LENGTH) return null;
    let url: URL;
    try {
        url = new URL(text);
    } catch {
        return null;
    }
    if ((url.protocol !== 'https:' && url.protocol !== 'http:') || url.username || url.password || url.port) {
        return null;
    }
    const segments = url.pathname.split('/').slice(1);
    // One trailing slash is how some of these addresses are copied.
    if (segments.length > 1 && segments[segments.length - 1] === '') segments.pop();

    switch (url.hostname) {
        case 'youtube.com':
        case 'www.youtube.com':
        case 'm.youtube.com':
            if (segments.length === 1 && segments[0] === 'watch') return youtube(oneParam(url, 'v'));
            if (segments.length === 2 && ['shorts', 'live', 'embed'].includes(segments[0])) return youtube(segments[1]);
            return null;
        case 'youtu.be':
            return segments.length === 1 ? youtube(segments[0]) : null;
        case 'www.youtube-nocookie.com':
            return segments.length === 2 && segments[0] === 'embed' ? youtube(segments[1]) : null;
        case 'vimeo.com':
        case 'www.vimeo.com':
            if (segments.length === 1) return vimeo(segments[0], oneParam(url, 'h'));
            if (segments.length === 2) return vimeo(segments[0], segments[1]);
            return null;
        case 'player.vimeo.com':
            return segments.length === 2 && segments[0] === 'video' ? vimeo(segments[1], oneParam(url, 'h')) : null;
        default:
            return null;
    }
}

/** Whether a Drive file's type says it is a video. Twin of `is_video_mime`. */
export function isVideoMime(mimeType: string): boolean {
    const type = mimeType.trim().toLowerCase();
    return type.startsWith('video/') && type.length > 'video/'.length;
}

/**
 * The three origins a media page may frame: the same list as `EMBED_ORIGINS`
 * in `backend/src/utils/media.rs` and as the public image's
 * Content-Security-Policy (`CSP_MEDIA_EMBEDS` in `frontend/Dockerfile`).
 */
export const MEDIA_EMBED_ORIGINS = [
    'https://www.youtube-nocookie.com',
    'https://player.vimeo.com',
    'https://drive.google.com',
] as const;

/**
 * The address the page frames for `src`, or `null` when it may frame nothing:
 * an `https://` address on exactly one of the three embed hosts, in the URL
 * parser's own spelling. The server builds the address; this is the page's
 * own check, so that no answer, however it was produced, frames another site.
 *
 * Both halves matter. `https:player.vimeo.com/video/1` parses as Vimeo's
 * player, but as a frame's `src` a browser resolves it against this page and
 * frames our own site. So the text must be written `https://`, and what is
 * framed is the parsed address, never the text.
 */
export function embedHref(src: unknown): string | null {
    if (typeof src !== 'string' || /\p{Cc}/u.test(src) || !src.startsWith('https://')) return null;
    let url: URL;
    try {
        url = new URL(src);
    } catch {
        return null;
    }
    return url.protocol === 'https:'
        && url.port === ''
        && url.username === ''
        && url.password === ''
        && (MEDIA_EMBED_ORIGINS as readonly string[]).includes(url.origin)
        ? url.href
        : null;
}

/** Whether the page may frame `src`. What it frames is `embedHref(src)`. */
export function isAllowedEmbedSrc(src: unknown): src is string {
    return embedHref(src) !== null;
}

/**
 * The address a button points at for `url`, or `null` when it may not be a
 * button's link: `https` with a host and no name or password, in the URL
 * parser's own spelling. Never `javascript:`. Twin of `cta_href` in
 * `backend/src/utils/media.rs`, which stores a link in the same spelling.
 *
 * `https:example.org` and `https:/example.org` parse as `https://example.org/`
 * but, as typed, an anchor resolves them against the page it is on. So an
 * anchor is given this address, never the text.
 */
export function httpsHref(url: unknown): string | null {
    if (typeof url !== 'string' || /\p{Cc}/u.test(url)) return null;
    try {
        const parsed = new URL(url.trim());
        return parsed.protocol === 'https:' && parsed.hostname !== '' && !parsed.username && !parsed.password
            && parsed.href.length <= MAX_URL_LENGTH
            ? parsed.href
            : null;
    } catch {
        return null;
    }
}

/** Whether `url` is an `https` address a button may point at. */
export function isHttpsUrl(url: unknown): url is string {
    return httpsHref(url) !== null;
}

/** The longest label of the button under the player. Twin of `MAX_CTA_LABEL_CHARS`. */
export const CTA_LABEL_CHARS = 40;

/**
 * Why the server would refuse this button, in words for the person, or `''`
 * when it would not. Twin of `cta_text` in `backend/src/utils/media.rs`. Run
 * before anything is sent, and before a Drive video's sharing is changed.
 */
export function ctaProblem(label: string, url: string): string {
    const text = label.trim();
    const link = url.trim();
    if (!text && !link) return '';
    if (!text || !link) return 'Give the button both a label and a link, or leave both empty.';
    if (Array.from(text).length > CTA_LABEL_CHARS) return `The button's label must be at most ${CTA_LABEL_CHARS} characters.`;
    if (/\p{Cc}/u.test(text)) return 'The button\'s label contains characters that cannot be used.';
    if (!isHttpsUrl(link) || link.length > MAX_URL_LENGTH) return 'The button\'s link must be an https:// address.';
    return '';
}

/** The `media` part of `POST /links` for this button; nothing when it is empty. */
export function ctaFields(label: string, url: string): { cta_label?: string; cta_url?: string } {
    const text = label.trim();
    const link = url.trim();
    return text && link ? { cta_label: text, cta_url: link } : {};
}
