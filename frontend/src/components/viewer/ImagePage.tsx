import { useEffect, useRef, useState } from 'react';
import { drawWatermark } from './watermark';

interface ImagePageProps {
    /**
     * The image's bytes. The page holds them for as long as the viewer is
     * open (they are this component's input, and a change of watermark draws
     * them again); nothing here writes them to storage or leaves an address
     * to them behind.
     */
    bytes: ArrayBuffer;
    contentType: string;
    /** Drawn into the canvas over the picture. */
    watermark: string;
    title: string;
}

/**
 * An image drawn on a canvas, with the watermark in its pixels. There is no
 * `<img>` in the page: nothing to drag out, and "Save image as" has nothing to
 * save. The object URL the picture is decoded from is revoked as soon as it
 * has been drawn.
 */
export default function ImagePage({ bytes, contentType, watermark, title }: ImagePageProps) {
    const canvas = useRef<HTMLCanvasElement>(null);
    const [failed, setFailed] = useState(false);

    useEffect(() => {
        const target = canvas.current;
        if (!target) return;
        let cancelled = false;
        const url = URL.createObjectURL(new Blob([bytes], { type: contentType }));
        const picture = new Image();
        picture.onload = () => {
            URL.revokeObjectURL(url);
            if (cancelled) return;
            target.width = picture.naturalWidth;
            target.height = picture.naturalHeight;
            target.getContext('2d')?.drawImage(picture, 0, 0);
            drawWatermark(target, watermark);
        };
        picture.onerror = () => {
            URL.revokeObjectURL(url);
            if (!cancelled) setFailed(true);
        };
        picture.src = url;
        return () => {
            cancelled = true;
            URL.revokeObjectURL(url);
            target.width = 0;
            target.height = 0;
        };
    }, [bytes, contentType, watermark]);

    if (failed) {
        return <p className="py-16 text-center text-sm text-muted">This image could not be shown.</p>;
    }
    return (
        <div className="flex justify-center">
            <canvas
                ref={canvas}
                aria-label={title}
                className="block h-auto max-w-full bg-white shadow-subtle"
            />
        </div>
    );
}
