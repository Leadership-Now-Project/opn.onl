import { useState, type KeyboardEvent } from 'react';
import { Plus } from 'lucide-react';
import { API_ENDPOINTS, authFetch, responseError } from '../../config/api';
import { catalogItems, type CatalogItem } from './linkScope';

const MAX_NAME = 100;

/**
 * "+ New folder" / "+ New tag" right where a link is filed, so a folder or tag
 * can be made without leaving the form for the dashboard's Manage menu (which
 * still renames, recolours and deletes them).
 */
export default function QuickAddCatalog({
    kind,
    orgId,
    onAdded,
}: {
    kind: 'folder' | 'tag';
    orgId: number | null;
    /** Called with the new item once the server has created it. */
    onAdded: (item: CatalogItem) => void;
}) {
    const [open, setOpen] = useState(false);
    const [name, setName] = useState('');
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState('');
    const noun = kind === 'folder' ? 'folder' : 'tag';

    const add = async () => {
        const trimmed = name.trim();
        if (!trimmed || busy) return;
        setBusy(true);
        setError('');
        const body: Record<string, unknown> = { name: trimmed };
        if (orgId != null) body.org_id = orgId;
        try {
            const res = await authFetch(kind === 'folder' ? API_ENDPOINTS.folders : API_ENDPOINTS.tags, {
                method: 'POST',
                body: JSON.stringify(body),
            });
            if (!res.ok) {
                setError(await responseError(res, `Failed to add ${noun}`));
                return;
            }
            const [item] = catalogItems([await res.json()]);
            setName('');
            setOpen(false);
            if (item) onAdded(item);
        } catch {
            setError(`Failed to add ${noun}`);
        } finally {
            setBusy(false);
        }
    };

    // Enter adds the folder or tag; it must not submit the link form around it.
    const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
        if (event.key === 'Enter') {
            event.preventDefault();
            void add();
        } else if (event.key === 'Escape') {
            setOpen(false);
        }
    };

    if (!open) {
        return (
            <button
                type="button"
                onClick={() => setOpen(true)}
                className="mt-2 inline-flex items-center gap-1 text-sm font-medium text-primary-600 hover:text-primary-700"
            >
                <Plus className="h-3.5 w-3.5" /> New {noun}
            </button>
        );
    }

    return (
        <div className="mt-2">
            <div className="flex gap-2">
                <label htmlFor={`quick-add-${kind}`} className="sr-only">New {noun} name</label>
                <input
                    id={`quick-add-${kind}`}
                    autoFocus
                    value={name}
                    maxLength={MAX_NAME}
                    onChange={(event) => setName(event.target.value)}
                    onKeyDown={onKeyDown}
                    placeholder={`New ${noun} name`}
                    className="min-w-0 flex-1 rounded-lg border border-line2 bg-surface px-3 py-1.5 text-sm text-ink outline-none transition-colors focus:border-primary-500 placeholder:text-faint"
                />
                <button
                    type="button"
                    onClick={() => void add()}
                    disabled={busy || !name.trim()}
                    className="rounded-lg bg-primary-600 px-3 py-1.5 text-sm font-semibold text-white transition-colors hover:bg-primary-700 disabled:opacity-60"
                >
                    Add
                </button>
                <button
                    type="button"
                    onClick={() => { setOpen(false); setError(''); }}
                    className="rounded-lg border border-line2 px-3 py-1.5 text-sm text-muted hover:text-ink"
                >
                    Cancel
                </button>
            </div>
            {error && <p role="alert" className="mt-1 text-xs text-danger">{error}</p>}
        </div>
    );
}
