/**
 * Whether `url` is an `http`/`https` address written out the ordinary way:
 * the scheme, `://`, then the host, with no backslash standing in for a slash
 * and no space after it.
 *
 * A URL parser is more forgiving. It reads `https:host/x`, `https:/host/x`
 * and `https:\\host\x` all as `https://host/x`, but a browser sent the typed
 * text by a short link would open a page of the short-link site instead. Our
 * server stores the parser's spelling for such text; the forms ask for it to
 * be typed in full, since a missing slash is nearly always a slip.
 *
 * Twin of `is_written_out_http_url` in `backend/src/utils/url_policy.rs`.
 * Change the two together.
 */
export function isWrittenOutHttpUrl(url: string): boolean {
    const match = /^https?:\/\/(.*)$/is.exec(url);
    if (!match) return false;
    const rest = match[1];
    // A backslash after `?` or `#` is data, and the parser leaves it alone.
    const hostAndPath = rest.split(/[?#]/, 1)[0];
    return hostAndPath !== ''
        && !rest.startsWith('/')
        && !hostAndPath.includes('\\')
        && !url.endsWith(' ');
}

export const DESTINATION_NOT_WRITTEN_OUT =
    'Write the destination in full, with two slashes after the colon, like https://example.com/page';

/**
 * What is wrong with the way a destination is written, or `null`. It only
 * speaks up for an http(s) address that is not written out; every other rule
 * (a host, no internal address, the blocklist) is the server's to apply.
 */
export function destinationProblem(url: string): string | null {
    const typed = url.trim();
    return /^https?:/i.test(typed) && !isWrittenOutHttpUrl(typed) ? DESTINATION_NOT_WRITTEN_OUT : null;
}
