import { useEffect, useRef, useState } from 'react';
import type { PageSize, PdfHandle } from './pdfDocument';
import { drawWatermark } from './watermark';

interface PdfPagesProps {
    pdf: PdfHandle;
    /** Drawn into every page's canvas. */
    watermark: string;
}

/** Pages are drawn at most this wide, in CSS pixels. */
const MAX_PAGE_WIDTH = 1000;
/** A page is drawn once it is this close to the window, and let go beyond it. */
const NEARBY = '1200px 0px';
/** Until a page's own size is known it takes the first page's shape. */
const LETTER: PageSize = { width: 612, height: 792 };

/**
 * The pages of a PDF, each a canvas. Only the pages near the window are
 * drawn, and a page scrolled far away gives its pixels back, so a document of
 * several hundred pages costs a few pages' memory.
 */
export default function PdfPages({ pdf, watermark }: PdfPagesProps) {
    const [firstSize, setFirstSize] = useState<PageSize>(LETTER);

    useEffect(() => {
        let cancelled = false;
        pdf.pageSize(1)
            .then((size) => { if (!cancelled) setFirstSize(size); })
            .catch(() => undefined);
        return () => { cancelled = true; };
    }, [pdf]);

    return (
        <div className="flex flex-col items-center gap-4">
            {Array.from({ length: pdf.pageCount }, (_, index) => (
                <PdfPage
                    key={index + 1}
                    pdf={pdf}
                    pageNumber={index + 1}
                    watermark={watermark}
                    fallbackSize={firstSize}
                />
            ))}
        </div>
    );
}

interface PdfPageProps extends PdfPagesProps {
    pageNumber: number;
    fallbackSize: PageSize;
}

function PdfPage({ pdf, pageNumber, watermark, fallbackSize }: PdfPageProps) {
    const frame = useRef<HTMLDivElement>(null);
    const canvas = useRef<HTMLCanvasElement>(null);
    // Near the window or not. Without IntersectionObserver every page counts
    // as near: the document still shows, only less frugally.
    const [near, setNear] = useState(() => typeof IntersectionObserver === 'undefined');
    const [size, setSize] = useState<PageSize | null>(null);
    const [failed, setFailed] = useState(false);

    useEffect(() => {
        const element = frame.current;
        if (!element || typeof IntersectionObserver === 'undefined') return;
        const observer = new IntersectionObserver(
            (entries) => {
                for (const entry of entries) setNear(entry.isIntersecting);
            },
            { rootMargin: NEARBY },
        );
        observer.observe(element);
        return () => observer.disconnect();
    }, []);

    // One canvas takes one drawing at a time: PDF.js refuses a second render
    // into a canvas that is still being drawn. Every draw and every release
    // waits for the one before it.
    const queue = useRef<Promise<void>>(Promise.resolve());

    useEffect(() => {
        const target = canvas.current;
        const element = frame.current;
        if (!target || !element) return;
        let cancelled = false;
        queue.current = queue.current.then(async () => {
            if (cancelled) return;
            if (!near) {
                // Give the pixels back. The frame keeps the page's place.
                target.width = 0;
                target.height = 0;
                return;
            }
            try {
                const pageSize = await pdf.pageSize(pageNumber);
                if (cancelled) return;
                setSize(pageSize);
                const cssWidth = Math.min(element.clientWidth || MAX_PAGE_WIDTH, MAX_PAGE_WIDTH);
                await pdf.renderPage(pageNumber, target, cssWidth);
                // The watermark goes on even if the page has since scrolled
                // away: a drawn page is never left without one.
                drawWatermark(target, watermark);
                if (!cancelled) setFailed(false);
            } catch {
                if (!cancelled) setFailed(true);
            }
        });
        return () => { cancelled = true; };
    }, [near, pdf, pageNumber, watermark]);

    const shape = size ?? fallbackSize;
    return (
        <div
            ref={frame}
            data-page={pageNumber}
            className="relative w-full bg-white shadow-subtle"
            style={{ maxWidth: MAX_PAGE_WIDTH, aspectRatio: `${shape.width} / ${shape.height}` }}
        >
            <canvas
                ref={canvas}
                aria-label={`Page ${pageNumber} of ${pdf.pageCount}`}
                className="block h-full w-full"
            />
            {failed && (
                <p className="absolute inset-0 flex items-center justify-center text-sm text-muted">
                    This page could not be shown.
                </p>
            )}
        </div>
    );
}
