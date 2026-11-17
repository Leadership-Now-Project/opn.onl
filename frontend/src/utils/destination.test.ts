import { describe, expect, it } from 'vitest';
import { DESTINATION_NOT_WRITTEN_OUT, destinationProblem, isWrittenOutHttpUrl } from './destination';

// The same lists as `url_policy.rs` tests on the server: the two must agree.
const NOT_WRITTEN_OUT = [
    'https:iana.org/probe',
    'https:/iana.org/probe',
    'https:///iana.org/probe',
    'https:////iana.org/probe',
    'http:iana.org',
    'http:/iana.org/a?b=1#c',
    'https:\\\\iana.org\\probe',
    'https:/\\iana.org/probe',
    'https://iana.org\\probe',
    'https://iana.org/a\\b',
    'HTTPS:IANA.org/Probe',
    'https://iana.org/probe ',
];

const WRITTEN_OUT = [
    'https://iana.org',
    'http://iana.org',
    'https://IANA.org/Probe',
    'HTTPS://iana.org/probe',
    'https://iana.org:443/probe',
    'https://bücher.de/a',
    'https://iana.org/a b',
    'https://iana.org/probe?x=a\\b#c\\d',
    'https://user@iana.org/probe',
];

describe('isWrittenOutHttpUrl', () => {
    it.each(WRITTEN_OUT)('takes %j as written out', (url) => {
        expect(isWrittenOutHttpUrl(url)).toBe(true);
    });

    it.each(NOT_WRITTEN_OUT)('does not take %j as written out, though it parses', (url) => {
        expect(new URL(url).hostname.toLowerCase()).toBe('iana.org');
        expect(isWrittenOutHttpUrl(url)).toBe(false);
    });

    it.each(['', 'iana.org', '//iana.org/probe', 'https:', 'https://', 'https:///', 'ftp:iana.org', 'opn-file:12', ' https://iana.org/'])(
        'does not take %j as written out',
        (url) => {
            expect(isWrittenOutHttpUrl(url)).toBe(false);
        },
    );
});

describe('destinationProblem', () => {
    it.each(WRITTEN_OUT)('finds nothing wrong with %j', (url) => {
        expect(destinationProblem(url)).toBeNull();
    });

    it.each(NOT_WRITTEN_OUT.filter((url) => !url.endsWith(' ')))('asks for %j to be written in full', (url) => {
        expect(destinationProblem(url)).toBe(DESTINATION_NOT_WRITTEN_OUT);
    });

    it('ignores spaces around the address, which the server drops', () => {
        expect(destinationProblem('  https://iana.org/probe  ')).toBeNull();
        expect(destinationProblem('  https:iana.org/probe  ')).toBe(DESTINATION_NOT_WRITTEN_OUT);
    });

    it('leaves text that is no http address to the server', () => {
        expect(destinationProblem('')).toBeNull();
        expect(destinationProblem('iana.org')).toBeNull();
        expect(destinationProblem('ftp://iana.org')).toBeNull();
    });
});
