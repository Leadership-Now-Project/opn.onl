import { describe, expect, it, vi } from 'vitest';
import { drawWatermark, watermarkText } from './watermark';

describe('watermarkText', () => {
    it('is the email and the minute in UTC', () => {
        expect(watermarkText('pat@example.org', '2026-11-13T14:05:42Z')).toBe('pat@example.org · 2026-11-13 14:05 UTC');
    });

    it('is in UTC whatever offset the time was written with', () => {
        expect(watermarkText('pat@example.org', '2026-11-13T23:30:00-05:00')).toBe('pat@example.org · 2026-11-14 04:30 UTC');
    });

    it('is still the email when the time cannot be read', () => {
        expect(watermarkText('pat@example.org', 'not a time')).toBe('pat@example.org');
    });
});

function fakeCanvas(width: number, height: number) {
    const context = {
        save: vi.fn(),
        restore: vi.fn(),
        translate: vi.fn(),
        rotate: vi.fn(),
        measureText: vi.fn(() => ({ width: 300 })),
        fillText: vi.fn(),
        font: '',
        textBaseline: '',
        fillStyle: '',
    };
    const canvas = { width, height, getContext: vi.fn(() => context) } as unknown as HTMLCanvasElement;
    return { canvas, context };
}

describe('drawWatermark', () => {
    it('tiles the text across the whole page, slanted, and puts the canvas back as it was', () => {
        const { canvas, context } = fakeCanvas(1200, 1600);
        drawWatermark(canvas, 'pat@example.org · 2026-11-13 14:05 UTC');

        expect(context.rotate).toHaveBeenCalledTimes(1);
        expect(context.rotate.mock.calls[0][0]).toBeLessThan(0);
        // Many copies, every one of them the text.
        expect(context.fillText.mock.calls.length).toBeGreaterThan(40);
        for (const [text] of context.fillText.mock.calls) {
            expect(text).toBe('pat@example.org · 2026-11-13 14:05 UTC');
        }
        // The grid reaches past every corner of the rotated page.
        const reach = Math.hypot(1200, 1600);
        const xs = context.fillText.mock.calls.map(call => call[1] as number);
        const ys = context.fillText.mock.calls.map(call => call[2] as number);
        expect(Math.min(...xs)).toBeLessThanOrEqual(-reach + 2);
        expect(Math.max(...ys)).toBeGreaterThan(reach - 200);
        expect(context.save).toHaveBeenCalledTimes(1);
        expect(context.restore).toHaveBeenCalledTimes(1);
    });

    it('does nothing without a context, a size or a text', () => {
        const none = { width: 10, height: 10, getContext: () => null } as unknown as HTMLCanvasElement;
        expect(() => drawWatermark(none, 'x')).not.toThrow();

        const empty = fakeCanvas(0, 0);
        drawWatermark(empty.canvas, 'x');
        expect(empty.context.fillText).not.toHaveBeenCalled();

        const blank = fakeCanvas(100, 100);
        drawWatermark(blank.canvas, '');
        expect(blank.context.fillText).not.toHaveBeenCalled();
    });
});
