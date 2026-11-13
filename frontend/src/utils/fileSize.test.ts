import { describe, expect, it } from 'vitest';
import { formatBytes } from './fileSize';

describe('formatBytes', () => {
    it('counts bytes below a kilobyte', () => {
        expect(formatBytes(0)).toBe('0 bytes');
        expect(formatBytes(1)).toBe('1 byte');
        expect(formatBytes(830)).toBe('830 bytes');
        expect(formatBytes(1023)).toBe('1023 bytes');
    });

    it('uses binary units with one decimal below 100', () => {
        expect(formatBytes(1024)).toBe('1 KB');
        expect(formatBytes(1536)).toBe('1.5 KB');
        expect(formatBytes(13_002_342)).toBe('12.4 MB');
        expect(formatBytes(1024 * 1024)).toBe('1 MB');
        expect(formatBytes(3 * 1024 ** 3)).toBe('3 GB');
    });

    it('drops the decimal from 100 up', () => {
        expect(formatBytes(524_288_000)).toBe('500 MB');
        expect(formatBytes(150.4 * 1024)).toBe('150 KB');
        // Rounds to 100 rather than reading "100.0".
        expect(formatBytes(99.96 * 1024)).toBe('100 KB');
    });

    it('moves up a unit at 1024, not at 1000', () => {
        expect(formatBytes(1000 * 1024)).toBe('1000 KB');
        expect(formatBytes(1024 * 1024 - 1)).toBe('1024 KB');
    });

    it('gives nothing for a value that is not a size', () => {
        expect(formatBytes(-1)).toBe('');
        expect(formatBytes(Number.NaN)).toBe('');
    });
});
