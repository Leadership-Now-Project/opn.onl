/**
 * The watermark of the Secure View viewer: the email the visitor gave and when
 * they opened the document, tiled across every page. It is drawn into each
 * page's canvas (so a saved or screenshotted page carries it) and again as an
 * overlay on top.
 */

/** The outline: white, well short of opaque. */
export const WATERMARK_OUTLINE = 'rgba(255, 255, 255, 0.5)';
/** The letters: a slate grey, light enough to read through. Never black. */
export const WATERMARK_FILL = 'rgba(40, 48, 66, 0.26)';

/** "pat@example.org · 2026-11-13 14:05 UTC". Minute precision, always UTC. */
export function watermarkText(email: string, at: string): string {
    const when = new Date(at);
    if (Number.isNaN(when.getTime())) return email;
    const stamp = when.toISOString().slice(0, 16).replace('T', ' ');
    return `${email} · ${stamp} UTC`;
}

/**
 * Tile `text` diagonally across the whole canvas, over whatever is drawn on
 * it. Does nothing where a canvas has no 2D context.
 */
export function drawWatermark(canvas: HTMLCanvasElement, text: string): void {
    const context = canvas.getContext('2d');
    if (!context || !text) return;
    const { width, height } = canvas;
    if (width === 0 || height === 0) return;

    // Sized from the page, so a phone and a wide monitor get the same density.
    const fontSize = Math.max(12, Math.round(width / 42));
    context.save();
    context.font = `600 ${fontSize}px Helvetica, Arial, sans-serif`;
    context.textBaseline = 'middle';
    context.lineJoin = 'round';
    context.lineWidth = Math.max(2, fontSize / 7);
    context.strokeStyle = WATERMARK_OUTLINE;
    context.fillStyle = WATERMARK_FILL;
    const stepX = context.measureText(text).width + fontSize * 4;
    const stepY = fontSize * 7;
    // Rotated about the centre; the grid is drawn large enough to cover the
    // corners of the rotated page.
    const reach = Math.hypot(width, height);
    context.translate(width / 2, height / 2);
    context.rotate(-Math.PI / 6);
    let row = 0;
    for (let y = -reach; y <= reach; y += stepY, row += 1) {
        // Every other row is shifted, so no straight band is left clear.
        const shift = row % 2 === 0 ? 0 : stepX / 2;
        for (let x = -reach - shift; x <= reach; x += stepX) {
            // A soft white outline under a light grey fill. On white paper
            // the outline disappears and only the grey shows; on a dark or
            // busy photograph the outline is what keeps the letters legible.
            // Both stay faint on purpose: the watermark must not get in the
            // way of reading.
            context.strokeText(text, x, y);
            context.fillText(text, x, y);
        }
    }
    context.restore();
}
