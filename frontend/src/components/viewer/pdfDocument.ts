/**
 * The only file that imports PDF.js. The viewer page loads it on demand, so
 * PDF.js is in no other page's bundle, and the tests replace it whole (jsdom
 * has no canvas to render into).
 *
 * How PDF.js is set up here, and why:
 *
 * - The legacy build, for visitors on browsers a few years old. The viewer is
 *   opened by the public.
 * - The worker is bundled by Vite as a same-origin script (`?worker`), which
 *   the site's Content-Security-Policy allows as it stands.
 * - `useWasm: false`. PDF.js's WebAssembly decoders would need
 *   `'wasm-unsafe-eval'` in the policy. Without them it uses its own
 *   JavaScript decoders for JBIG2 and JPEG 2000 images, fetched from
 *   `/pdfjs/wasm/` only when a document needs one (`vite.config.ts` copies
 *   them there). The one thing lost is ICC colour management: such colours
 *   are approximated.
 * - This version of PDF.js never evaluates strings, so there is no
 *   `isEvalSupported` to switch off any more.
 * - The bytes are handed over as `data`. PDF.js is given no URL and makes no
 *   request for the document itself.
 * - No text layer and no annotation layer are ever built: a page is pixels on
 *   a canvas, with nothing to select or search in the DOM.
 */

import * as pdfjs from 'pdfjs-dist/legacy/build/pdf.mjs';
import PdfWorker from 'pdfjs-dist/legacy/build/pdf.worker.min.mjs?worker';

export interface PageSize {
    width: number;
    height: number;
}

export interface PdfHandle {
    pageCount: number;
    /** The page's size in CSS pixels at scale 1 (page numbers start at 1). */
    pageSize(pageNumber: number): Promise<PageSize>;
    /** Draw the page into `canvas`, `cssWidth` CSS pixels wide. */
    renderPage(pageNumber: number, canvas: HTMLCanvasElement, cssWidth: number): Promise<void>;
    /** Stop the worker and drop the document. */
    destroy(): Promise<void>;
}

/** Where the files PDF.js fetches on demand are served from. */
function resourceUrl(folder: string): string {
    return new URL(`${import.meta.env.BASE_URL}pdfjs/${folder}/`, window.location.origin).href;
}

/** Sharp on a high-density screen, without quadrupling a phone's memory use. */
function pixelRatio(): number {
    return Math.min(window.devicePixelRatio || 1, 2);
}

export async function openPdf(data: Uint8Array): Promise<PdfHandle> {
    const port = new PdfWorker();
    const worker = pdfjs.PDFWorker.create({ port });
    const task = pdfjs.getDocument({
        data,
        worker,
        useWasm: false,
        wasmUrl: resourceUrl('wasm'),
        cMapUrl: resourceUrl('cmaps'),
        cMapPacked: true,
        standardFontDataUrl: resourceUrl('standard_fonts'),
        disableWorkerRendering: true,
        enableXfa: false,
    });
    let document: Awaited<typeof task.promise>;
    try {
        document = await task.promise;
    } catch (error) {
        await task.destroy().catch(() => undefined);
        port.terminate();
        throw error;
    }

    return {
        pageCount: document.numPages,
        async pageSize(pageNumber) {
            const page = await document.getPage(pageNumber);
            const viewport = page.getViewport({ scale: 1 });
            return { width: viewport.width, height: viewport.height };
        },
        async renderPage(pageNumber, canvas, cssWidth) {
            const page = await document.getPage(pageNumber);
            const base = page.getViewport({ scale: 1 });
            const viewport = page.getViewport({ scale: (cssWidth / base.width) * pixelRatio() });
            canvas.width = Math.floor(viewport.width);
            canvas.height = Math.floor(viewport.height);
            await page.render({ canvas, viewport }).promise;
            page.cleanup();
        },
        async destroy() {
            await task.destroy().catch(() => undefined);
            port.terminate();
        },
    };
}
