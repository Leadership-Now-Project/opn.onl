import { useRef, type KeyboardEvent, type ReactNode } from 'react';
import CreateMediaTab from './CreateMediaTab';

export type CreateTab = 'links' | 'files' | 'media';

const TABS: { id: CreateTab; label: string }[] = [
    { id: 'links', label: 'Links' },
    { id: 'files', label: 'Files' },
    { id: 'media', label: 'Media' },
];

interface CreateCardProps {
    tab: CreateTab;
    onTabChange: (tab: CreateTab) => void;
    /** The Links panel: the create form. */
    children: ReactNode;
    /** The Files panel: upload a file and share it. */
    files: ReactNode;
}

/**
 * The create card with its Links / Files / Media tabs. Every panel stays
 * mounted and the ones not chosen are `hidden`, so what was typed into the
 * link form is still there after a look at another tab.
 */
export default function CreateCard({ tab, onTabChange, children, files }: CreateCardProps) {
    const tabRefs = useRef<Partial<Record<CreateTab, HTMLButtonElement | null>>>({});

    // Arrow keys move between tabs and choose the one they land on; Tab leaves
    // the list for the panel (the WAI-ARIA tabs pattern, automatic activation).
    const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
        const index = TABS.findIndex(t => t.id === tab);
        let next: number;
        switch (event.key) {
            case 'ArrowRight': next = (index + 1) % TABS.length; break;
            case 'ArrowLeft': next = (index - 1 + TABS.length) % TABS.length; break;
            case 'Home': next = 0; break;
            case 'End': next = TABS.length - 1; break;
            default: return;
        }
        event.preventDefault();
        const id = TABS[next].id;
        onTabChange(id);
        tabRefs.current[id]?.focus();
    };

    const panels: Record<CreateTab, ReactNode> = {
        links: children,
        files,
        media: <CreateMediaTab />,
    };

    return (
        <div className="rounded-2xl border border-line2 bg-surface p-6 shadow-subtle lg:max-h-[calc(100vh-7rem)] lg:overflow-y-auto">
            <h2 id="create-card-heading" className="font-display text-lg font-bold text-ink tracking-tight mb-4">Create new link</h2>
            <div role="tablist" aria-labelledby="create-card-heading" className="mb-5 flex border-b border-line2">
                {TABS.map(({ id, label }) => {
                    const selected = id === tab;
                    return (
                        <button
                            key={id}
                            ref={(el) => { tabRefs.current[id] = el; }}
                            type="button"
                            role="tab"
                            id={`create-tab-${id}`}
                            aria-selected={selected}
                            aria-controls={`create-panel-${id}`}
                            tabIndex={selected ? 0 : -1}
                            onClick={() => onTabChange(id)}
                            onKeyDown={onKeyDown}
                            className={`-mb-px flex-1 border-b-2 px-3 py-2 text-sm font-semibold transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary-500 ${
                                selected
                                    ? 'border-primary-600 text-ink'
                                    : 'border-transparent text-muted hover:text-ink'
                            }`}
                        >
                            {label}
                        </button>
                    );
                })}
            </div>
            {TABS.map(({ id }) => (
                <div
                    key={id}
                    role="tabpanel"
                    id={`create-panel-${id}`}
                    aria-labelledby={`create-tab-${id}`}
                    hidden={id !== tab}
                >
                    {panels[id]}
                </div>
            ))}
        </div>
    );
}
