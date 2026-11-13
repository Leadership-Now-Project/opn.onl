import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '../../test/test-utils';
import ImagePage from './ImagePage';
import { drawWatermark } from './watermark';

// Only the drawing is watched: jsdom has no 2D context to draw into.
vi.mock('./watermark', async (original) => ({
    ...(await original<typeof import('./watermark')>()),
    drawWatermark: vi.fn(),
}));

const WATERMARK = 'pat@example.org · 2026-11-13 14:05 UTC';

/** What happened to the canvas, in order. */
let drawn: string[];
/** How the next picture ends: decoded, or refused by the browser. */
let decodes: boolean;

/** jsdom never loads an image; this one "loads" as soon as it is given an address. */
class FakeImage {
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    naturalWidth = 640;
    naturalHeight = 480;
    set src(_address: string) {
        queueMicrotask(() => (decodes ? this.onload?.() : this.onerror?.()));
    }
}

describe('ImagePage', () => {
    beforeEach(() => {
        drawn = [];
        decodes = true;
        vi.mocked(drawWatermark).mockReset();
        vi.mocked(drawWatermark).mockImplementation(() => {
            drawn.push('watermark');
        });
        vi.stubGlobal('Image', FakeImage);
        // jsdom has neither of these.
        URL.createObjectURL = vi.fn(() => 'blob:picture');
        URL.revokeObjectURL = vi.fn();
        vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(
            () =>
                ({
                    drawImage: () => {
                        drawn.push('picture');
                    },
                }) as unknown as CanvasRenderingContext2D,
        );
    });

    afterEach(() => {
        vi.unstubAllGlobals();
        vi.restoreAllMocks();
        Reflect.deleteProperty(URL, 'createObjectURL');
        Reflect.deleteProperty(URL, 'revokeObjectURL');
    });

    it('draws the watermark into the picture itself, on top of it', async () => {
        render(
            <ImagePage
                bytes={new Uint8Array([1, 2, 3]).buffer}
                contentType="image/png"
                watermark={WATERMARK}
                title="Site plan"
            />,
        );
        const canvas = screen.getByLabelText('Site plan') as HTMLCanvasElement;

        // For an image this is the one watermark that is in the pixels: what a
        // saved or copied canvas carries. The overlay on top is a separate
        // element and stays behind.
        await waitFor(() => expect(drawWatermark).toHaveBeenCalledTimes(1));
        expect(drawWatermark).toHaveBeenCalledWith(canvas, WATERMARK);
        expect(drawn).toEqual(['picture', 'watermark']);
        expect([canvas.width, canvas.height]).toEqual([640, 480]);

        // Nothing to drag out or "Save image as", and the address the picture
        // was decoded from is gone.
        expect(document.querySelector('img')).toBeNull();
        expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:picture');
    });

    it('says so when the picture cannot be shown, and draws nothing', async () => {
        decodes = false;
        render(
            <ImagePage
                bytes={new Uint8Array([0]).buffer}
                contentType="image/png"
                watermark={WATERMARK}
                title="Site plan"
            />,
        );
        expect(await screen.findByText('This image could not be shown.')).toBeVisible();
        expect(drawWatermark).not.toHaveBeenCalled();
        expect(drawn).toEqual([]);
    });
});
