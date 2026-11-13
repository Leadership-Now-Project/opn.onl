import { useEffect, useRef, useState, type FormEvent } from 'react';
import { useParams } from 'react-router-dom';
import { FileLock2, Loader2 } from 'lucide-react';
import { API_ENDPOINTS, shortLinkBase } from '../config/api';
import SEO from '../components/SEO';
import ImagePage from '../components/viewer/ImagePage';
import PdfPages from '../components/viewer/PdfPages';
import ProtectedDocument from '../components/viewer/ProtectedDocument';
import type { PdfHandle } from '../components/viewer/pdfDocument';
import { watermarkText } from '../components/viewer/watermark';
import logger from '../utils/logger';

/** What `POST /{code}/view` answers. */
interface View {
    view_token: string;
    expires_in: number;
    title: string;
    file: { kind: 'pdf' | 'image'; content_type: string; filename: string; size_bytes: number };
    watermark: { email: string; at: string };
}

type Stage =
    | { name: 'email' }
    /** No ticket, or one the server no longer takes: the link has to be opened again. */
    | { name: 'expired' }
    /** The link itself is over: expired, used up, switched off, file gone. */
    | { name: 'gone'; message: string }
    | { name: 'loading'; view: View }
    | { name: 'pdf'; view: View; pdf: PdfHandle }
    | { name: 'image'; view: View; bytes: ArrayBuffer }
    | { name: 'failed'; message: string };

/**
 * The ticket `GET /{code}` put in the address's fragment (`#t=...`). A
 * fragment is never sent to a server, so the ticket is in no access log.
 */
function ticketInAddress(): string {
    const match = /(?:^#|&)t=([^&]+)/.exec(window.location.hash);
    if (!match) return '';
    try {
        return decodeURIComponent(match[1]);
    } catch {
        return '';
    }
}

function sentence(reason: unknown, fallback: string): string {
    if (typeof reason !== 'string' || !reason.trim()) return fallback;
    return reason.endsWith('.') ? reason : `${reason}.`;
}

async function errorOf(response: Response): Promise<unknown> {
    try {
        return (await response.json())?.error;
    } catch {
        return undefined;
    }
}

const LINK_OVER = 'This link is no longer available.';

/**
 * The Secure View viewer: `/v/{code}`. A visitor lands here from the short
 * link with a ticket, gives an email address, and is shown the document with
 * that address and the time drawn across it.
 */
export default function SecureView() {
    const { code = '' } = useParams<{ code: string }>();
    // Read once, kept in memory only, and taken out of the address bar below.
    const [ticket] = useState(ticketInAddress);
    const [stage, setStage] = useState<Stage>(ticket ? { name: 'email' } : { name: 'expired' });
    const [email, setEmail] = useState('');
    const [formError, setFormError] = useState('');
    const [sending, setSending] = useState(false);
    const openPdfHandle = useRef<PdfHandle | null>(null);

    useEffect(() => {
        // So the ticket is not in the browser's history, a bookmark or a
        // copied address. history.state is the router's; it is passed through.
        if (window.location.hash) {
            const { pathname, search } = window.location;
            window.history.replaceState(window.history.state, '', pathname + search);
        }
    }, []);

    // Leaving the page stops PDF.js and drops the document.
    useEffect(() => () => {
        void openPdfHandle.current?.destroy();
        openPdfHandle.current = null;
    }, []);

    const loadFile = async (view: View) => {
        setStage({ name: 'loading', view });
        try {
            // The token travels in a header, never in the address.
            const response = await fetch(API_ENDPOINTS.viewFile(code), {
                headers: { 'X-View-Token': view.view_token },
                cache: 'no-store',
            });
            if (response.status === 401) {
                setStage({ name: 'expired' });
                return;
            }
            if (response.status === 410) {
                setStage({ name: 'gone', message: sentence(await errorOf(response), LINK_OVER) });
                return;
            }
            if (!response.ok) {
                setStage({ name: 'failed', message: 'The document could not be loaded. Try again in a moment.' });
                return;
            }
            const bytes = await response.arrayBuffer();
            if (view.file.kind === 'image') {
                setStage({ name: 'image', view, bytes });
                return;
            }
            // PDF.js is loaded only now, and only on this page.
            const { openPdf } = await import('../components/viewer/pdfDocument');
            const pdf = await openPdf(new Uint8Array(bytes));
            openPdfHandle.current = pdf;
            setStage({ name: 'pdf', view, pdf });
        } catch (error) {
            logger.error('Failed to load a protected document', error);
            setStage({ name: 'failed', message: 'The document could not be shown.' });
        }
    };

    const handleSubmit = async (event: FormEvent) => {
        event.preventDefault();
        if (sending) return;
        setFormError('');
        setSending(true);
        try {
            const response = await fetch(API_ENDPOINTS.viewOpen(code), {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ ticket, email }),
            });
            if (response.ok) {
                await loadFile(await response.json() as View);
            } else if (response.status === 400) {
                setFormError('Enter a valid email address.');
            } else if (response.status === 401) {
                setStage({ name: 'expired' });
            } else if (response.status === 410) {
                setStage({ name: 'gone', message: sentence(await errorOf(response), LINK_OVER) });
            } else if (response.status === 429) {
                const wait = response.headers.get('Retry-After');
                setFormError(`Too many attempts. Try again in ${wait || '60'} seconds.`);
            } else {
                setFormError('Something went wrong. Try again in a moment.');
            }
        } catch (error) {
            logger.error('Failed to open a protected document', error);
            setFormError('Network error. Check your connection and try again.');
        } finally {
            setSending(false);
        }
    };

    const again = `${shortLinkBase()}/${encodeURIComponent(code)}`;

    if (stage.name === 'pdf' || stage.name === 'image') {
        const { view } = stage;
        const mark = watermarkText(view.watermark.email, view.watermark.at);
        return (
            <div className="mx-auto max-w-5xl px-4 py-6">
                <SEO title={view.title} description="A protected document." noIndex />
                <div className="mb-4">
                    <h1 className="font-display text-xl font-bold tracking-tight text-ink break-words">{view.title}</h1>
                    <p className="mt-1 text-xs text-muted">
                        Shared for viewing only. Viewing as {view.watermark.email}.
                    </p>
                </div>
                <ProtectedDocument watermark={mark}>
                    {stage.name === 'pdf' ? (
                        <PdfPages pdf={stage.pdf} watermark={mark} />
                    ) : (
                        <ImagePage
                            bytes={stage.bytes}
                            contentType={view.file.content_type}
                            watermark={mark}
                            title={view.title}
                        />
                    )}
                </ProtectedDocument>
            </div>
        );
    }

    return (
        <div className="flex min-h-[70vh] items-center justify-center px-4 py-10">
            <SEO title="Protected document" description="A document shared for viewing only." noIndex />
            <div className="w-full max-w-md rounded-2xl border border-line2 bg-surface p-8 shadow-subtle">
                <div className="mb-5 flex h-12 w-12 items-center justify-center rounded-xl bg-primary-50">
                    <FileLock2 className="h-6 w-6 text-primary-600" aria-hidden="true" />
                </div>

                {stage.name === 'email' && (
                    <form onSubmit={handleSubmit} noValidate>
                        <h1 className="font-display text-xl font-bold tracking-tight text-ink">
                            Enter your email to view this document
                        </h1>
                        <p className="mt-2 text-sm text-muted">
                            Your email is shown on the document and is visible to the person who shared it.
                        </p>
                        <label htmlFor="viewer-email" className="mt-6 block text-sm font-medium text-ink">
                            Email address
                        </label>
                        <input
                            id="viewer-email"
                            type="email"
                            autoComplete="email"
                            inputMode="email"
                            required
                            autoFocus
                            maxLength={254}
                            value={email}
                            onChange={(e) => setEmail(e.target.value)}
                            aria-describedby={formError ? 'viewer-email-error' : undefined}
                            className="mt-1.5 w-full rounded-lg border border-line2 bg-surface px-4 py-2.5 text-sm text-ink outline-none transition-colors focus:border-primary-500"
                        />
                        {formError && (
                            <p id="viewer-email-error" role="alert" className="mt-2 text-sm text-danger">{formError}</p>
                        )}
                        <button
                            type="submit"
                            disabled={sending || !email.trim()}
                            className="mt-5 flex w-full items-center justify-center gap-2 rounded-lg bg-primary-600 px-4 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-primary-700 disabled:cursor-not-allowed disabled:opacity-60"
                        >
                            {sending && <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />}
                            View document
                        </button>
                    </form>
                )}

                {stage.name === 'loading' && (
                    <div role="status" className="flex items-center gap-3 text-sm text-muted">
                        <Loader2 className="h-5 w-5 animate-spin text-primary-600" aria-hidden="true" />
                        Loading the document…
                    </div>
                )}

                {stage.name === 'expired' && (
                    <div>
                        <h1 className="font-display text-xl font-bold tracking-tight text-ink">
                            This viewing session has expired
                        </h1>
                        <p className="mt-2 text-sm text-muted">
                            For your security a document can be opened for a few minutes at a time.
                        </p>
                        <a
                            href={again}
                            className="mt-5 inline-block rounded-lg bg-primary-600 px-4 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-primary-700"
                        >
                            Open the link again
                        </a>
                    </div>
                )}

                {stage.name === 'gone' && (
                    <div>
                        <h1 className="font-display text-xl font-bold tracking-tight text-ink">
                            This document is not available
                        </h1>
                        <p role="alert" className="mt-2 text-sm text-muted">{stage.message}</p>
                    </div>
                )}

                {stage.name === 'failed' && (
                    <div>
                        <h1 className="font-display text-xl font-bold tracking-tight text-ink">
                            Something went wrong
                        </h1>
                        <p role="alert" className="mt-2 text-sm text-muted">{stage.message}</p>
                        <a
                            href={again}
                            className="mt-5 inline-block rounded-lg bg-primary-600 px-4 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-primary-700"
                        >
                            Open the link again
                        </a>
                    </div>
                )}
            </div>
        </div>
    );
}
