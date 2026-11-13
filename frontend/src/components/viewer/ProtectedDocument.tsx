import { useEffect, useRef, useState, type ReactNode } from 'react';
import { EyeOff } from 'lucide-react';

interface ProtectedDocumentProps {
    /** The watermark text, repeated over the whole document. */
    watermark: string;
    children: ReactNode;
}

/** The overlay's tile: the text once, slanted, on a transparent square. */
function watermarkTile(text: string): string {
    const safe = text
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
    const svg =
        `<svg xmlns="http://www.w3.org/2000/svg" width="460" height="260">` +
        `<text x="230" y="130" text-anchor="middle" transform="rotate(-30 230 130)" ` +
        `font-family="Helvetica, Arial, sans-serif" font-size="15" font-weight="600" ` +
        `fill="rgb(20,28,48)" fill-opacity="0.13">${safe}</text></svg>`;
    // encodeURIComponent leaves ( ) ' ! * as they are; inside url(...) they
    // are safer encoded too.
    const encoded = encodeURIComponent(svg).replace(
        /[()'!*]/g,
        (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`,
    );
    return `url("data:image/svg+xml,${encoded}")`;
}

/**
 * What surrounds the document in the Secure View viewer. These are deterrents,
 * not locks: someone determined can still photograph the screen.
 *
 * - right-click, selecting, dragging, copying and cutting do nothing in here;
 * - Ctrl/Cmd+P and Ctrl/Cmd+S are swallowed, and printing the page prints
 *   nothing;
 * - the watermark lies over everything, on top of the one in each page;
 * - the document blurs behind a veil whenever the window is not the one in
 *   front, which is when most capture tools are used.
 */
export default function ProtectedDocument({ watermark, children }: ProtectedDocumentProps) {
    const root = useRef<HTMLDivElement>(null);
    const [veiled, setVeiled] = useState(false);

    useEffect(() => {
        const element = root.current;
        if (!element) return;
        const refuse = (event: Event) => event.preventDefault();
        const refused = ['contextmenu', 'selectstart', 'dragstart', 'copy', 'cut'];
        for (const name of refused) element.addEventListener(name, refuse);
        return () => {
            for (const name of refused) element.removeEventListener(name, refuse);
        };
    }, []);

    useEffect(() => {
        const onKey = (event: KeyboardEvent) => {
            const key = event.key.toLowerCase();
            if ((event.ctrlKey || event.metaKey) && (key === 'p' || key === 's')) {
                event.preventDefault();
            }
        };
        const veil = () => setVeiled(true);
        const unveil = () => setVeiled(false);
        const onVisibility = () => {
            if (document.visibilityState === 'hidden') setVeiled(true);
        };
        window.addEventListener('keydown', onKey, true);
        window.addEventListener('blur', veil);
        window.addEventListener('focus', unveil);
        window.addEventListener('beforeprint', veil);
        document.addEventListener('visibilitychange', onVisibility);
        return () => {
            window.removeEventListener('keydown', onKey, true);
            window.removeEventListener('blur', veil);
            window.removeEventListener('focus', unveil);
            window.removeEventListener('beforeprint', veil);
            document.removeEventListener('visibilitychange', onVisibility);
        };
    }, []);

    return (
        <div
            ref={root}
            data-testid="protected-document"
            className="relative"
            style={{ userSelect: 'none', WebkitUserSelect: 'none', WebkitTouchCallout: 'none' }}
        >
            {/* Printing the page prints nothing at all. */}
            <style>{'@media print { body { display: none !important; } }'}</style>

            <div
                data-testid="document-content"
                data-veiled={veiled ? 'true' : 'false'}
                aria-hidden={veiled}
                style={veiled ? { filter: 'blur(22px)' } : undefined}
            >
                {children}
            </div>

            <div
                aria-hidden="true"
                data-testid="watermark-overlay"
                data-watermark={watermark}
                className="pointer-events-none absolute inset-0"
                style={{ backgroundImage: watermarkTile(watermark), backgroundRepeat: 'repeat' }}
            />

            {veiled && (
                <button
                    type="button"
                    onClick={() => setVeiled(false)}
                    className="fixed inset-0 z-40 flex flex-col items-center justify-center gap-3 bg-paper/70 text-ink"
                >
                    <EyeOff className="h-8 w-8 text-muted" aria-hidden="true" />
                    <span className="text-base font-semibold">Click to continue viewing</span>
                </button>
            )}
        </div>
    );
}
