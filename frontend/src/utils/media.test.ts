import { describe, expect, it } from 'vitest';
import {
    ctaFields,
    ctaProblem,
    isAllowedEmbedSrc,
    isHttpsUrl,
    isVideoMime,
    parseMediaUrl,
    providerName,
} from './media';

// The same tables as the unit tests of `backend/src/utils/media.rs`: the two
// parsers are twins, and the server's is the one that decides.

describe('parseMediaUrl', () => {
    it.each([
        'https://www.youtube.com/watch?v=dQw4w9WgXcQ',
        'https://youtube.com/watch?v=dQw4w9WgXcQ',
        'http://www.youtube.com/watch?v=dQw4w9WgXcQ',
        'https://m.youtube.com/watch?v=dQw4w9WgXcQ',
        'https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ&list=PL123',
        'https://youtu.be/dQw4w9WgXcQ',
        'https://youtu.be/dQw4w9WgXcQ?si=AbCdEf&t=90',
        'https://www.youtube.com/shorts/dQw4w9WgXcQ',
        'https://www.youtube.com/live/dQw4w9WgXcQ',
        'https://www.youtube.com/live/dQw4w9WgXcQ/',
        'https://www.youtube.com/embed/dQw4w9WgXcQ',
        'https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ',
        '  https://youtu.be/dQw4w9WgXcQ  ',
    ])('reads the YouTube video in %s', (url) => {
        expect(parseMediaUrl(url)).toEqual({ provider: 'youtube', mediaId: 'dQw4w9WgXcQ' });
    });

    it.each([
        ['https://vimeo.com/76979871', '76979871'],
        ['https://www.vimeo.com/76979871', '76979871'],
        ['https://vimeo.com/76979871/', '76979871'],
        ['https://vimeo.com/76979871?share=copy', '76979871'],
        ['https://player.vimeo.com/video/76979871', '76979871'],
        ['https://vimeo.com/76979871/abc123def0', '76979871:abc123def0'],
        ['https://vimeo.com/76979871?h=abc123def0', '76979871:abc123def0'],
        ['https://player.vimeo.com/video/76979871?h=abc123def0', '76979871:abc123def0'],
    ])('reads the Vimeo video in %s', (url, mediaId) => {
        expect(parseMediaUrl(url)).toEqual({ provider: 'vimeo', mediaId });
    });

    it.each([
        '',
        'not a url',
        'youtube.com/watch?v=dQw4w9WgXcQ',
        'javascript:alert(1)//youtu.be/dQw4w9WgXcQ',
        'ftp://youtu.be/dQw4w9WgXcQ',
        'data:text/html,https://youtu.be/dQw4w9WgXcQ',
        'https://youtube.com.evil.test/watch?v=dQw4w9WgXcQ',
        'https://evil.test/watch?v=dQw4w9WgXcQ',
        'https://notyoutube.com/watch?v=dQw4w9WgXcQ',
        'https://youtu.be.evil.test/dQw4w9WgXcQ',
        'https://evil.test/?u=https://youtu.be/dQw4w9WgXcQ',
        'https://music.youtube.com/watch?v=dQw4w9WgXcQ',
        'https://yоutube.com/watch?v=dQw4w9WgXcQ',
        'https://vimeo.com.evil.test/76979871',
        'https://youtube.com@evil.test/watch?v=dQw4w9WgXcQ',
        'https://user@www.youtube.com/watch?v=dQw4w9WgXcQ',
        'https://user:pw@youtu.be/dQw4w9WgXcQ',
        'https://www.youtube.com:8443/watch?v=dQw4w9WgXcQ',
        'https://vimeo.com:444/76979871',
        'https://www.youtube.com/',
        'https://www.youtube.com/watch',
        'https://www.youtube.com/watch/dQw4w9WgXcQ',
        'https://www.youtube.com/playlist?list=PL1234567890',
        'https://www.youtube.com/channel/dQw4w9WgXcQ',
        'https://www.youtube.com/shorts/dQw4w9WgXcQ/extra',
        'https://www.youtube.com/embed/videoseries?list=PL1234567890',
        'https://www.youtube.com/embed/live_stream?channel=UC123',
        'https://youtu.be/dQw4w9WgXcQ/extra',
        'https://www.youtube-nocookie.com/watch?v=dQw4w9WgXcQ',
        'https://vimeo.com/channels/staffpicks/76979871',
        'https://vimeo.com/76979871/abc123def0/extra',
        'https://player.vimeo.com/76979871',
        'https://player.vimeo.com/video/76979871/extra',
        'https://youtu.be/dQw4w9WgXc',
        'https://youtu.be/dQw4w9WgXcQQ',
        'https://youtu.be/dQw4w9WgXc%51',
        'https://youtu.be/dQw4w9WgXc!',
        'https://youtu.be/dQw4w9WgXcé',
        'https://www.youtube.com/watch?v=dQw4w9WgXcQ&v=aaaaaaaaaaa',
        'https://vimeo.com/0123',
        'https://vimeo.com/1234567890123',
        'https://vimeo.com/76979871a',
        'https://vimeo.com/76979871/ABC123DEF0',
        'https://vimeo.com/76979871/abc',
        'https://vimeo.com/76979871?h=abc123def0&h=ffffffffff',
        'https://youtu.be/dQw4w9WgXcQ\n',
        'https://youtu.be/dQw4\tw9WgXcQ',
        `https://www.youtube.com/watch?v=dQw4w9WgXcQ&x=${'a'.repeat(2100)}`,
    ])('does not read %j', (url) => {
        expect(parseMediaUrl(url)).toBeNull();
    });
});

describe('isAllowedEmbedSrc', () => {
    it.each([
        'https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ',
        'https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?start=90',
        'https://player.vimeo.com/video/76979871?h=abc123def0',
        'https://drive.google.com/file/d/1AbC_d-9/preview',
    ])('allows %s', (src) => {
        expect(isAllowedEmbedSrc(src)).toBe(true);
    });

    it.each([
        'https://evil.test/embed/dQw4w9WgXcQ',
        'https://www.youtube-nocookie.com.evil.test/embed/dQw4w9WgXcQ',
        'https://evil.test/?https://www.youtube-nocookie.com/embed/x',
        'https://www.youtube-nocookie.com@evil.test/embed/x',
        'https://user:pw@player.vimeo.com/video/1',
        'http://www.youtube-nocookie.com/embed/dQw4w9WgXcQ',
        'https://www.youtube-nocookie.com:8443/embed/dQw4w9WgXcQ',
        'https://www.youtube.com/embed/dQw4w9WgXcQ',
        'https://docs.google.com/file/d/x/preview',
        '//player.vimeo.com/video/1',
        '/embed/dQw4w9WgXcQ',
        'javascript:alert(1)',
        'data:text/html,<script>alert(1)</script>',
        'https://player.vimeo.com/video/1\n',
        '',
        null,
        undefined,
        42,
        { src: 'https://player.vimeo.com/video/1' },
    ])('refuses %j', (src) => {
        expect(isAllowedEmbedSrc(src)).toBe(false);
    });
});

describe('the button', () => {
    it('is both a label and an https link, or neither', () => {
        expect(ctaProblem('', '')).toBe('');
        expect(ctaProblem('  ', ' ')).toBe('');
        expect(ctaProblem('Join us', 'https://example.org/join')).toBe('');
        expect(ctaProblem('Join us', '')).toMatch(/both a label and a link/);
        expect(ctaProblem('', 'https://example.org/join')).toMatch(/both a label and a link/);
        expect(ctaProblem('x'.repeat(41), 'https://example.org/')).toMatch(/40 characters/);
        expect(ctaProblem('x'.repeat(40), 'https://example.org/')).toBe('');
    });

    it.each([
        'javascript:alert(1)',
        'JaVaScRiPt:alert(1)',
        'data:text/html,<script>alert(1)</script>',
        'vbscript:msgbox(1)',
        'http://example.org/',
        '//example.org/',
        'example.org',
        'https://user:pw@example.org/',
        'mailto:someone@example.org',
    ])('refuses the link %j', (url) => {
        expect(ctaProblem('Go', url)).toMatch(/https:\/\//);
        expect(isHttpsUrl(url)).toBe(false);
    });

    it('is sent trimmed, and not at all when empty', () => {
        expect(ctaFields(' Join ', ' https://example.org/ ')).toEqual({ cta_label: 'Join', cta_url: 'https://example.org/' });
        expect(ctaFields('', '')).toEqual({});
    });
});

describe('small rules', () => {
    it('knows a video type', () => {
        expect(isVideoMime('video/mp4')).toBe(true);
        expect(isVideoMime(' Video/QuickTime ')).toBe(true);
        expect(isVideoMime('video/')).toBe(false);
        expect(isVideoMime('application/pdf')).toBe(false);
        expect(isVideoMime('xvideo/mp4')).toBe(false);
        expect(isVideoMime('')).toBe(false);
    });

    it('names a provider, and calls an unknown one a video', () => {
        expect(providerName('youtube')).toBe('YouTube');
        expect(providerName('vimeo')).toBe('Vimeo');
        expect(providerName('drive')).toBe('Google Drive');
        expect(providerName('zoom')).toBe('Video');
    });
});
