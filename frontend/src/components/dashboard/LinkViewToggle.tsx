import { useRef } from 'react';
import { IS_LNP } from '../../config/theme';
import type { LinkView } from './linkView';

const OPTIONS: { value: LinkView; label: string }[] = [
    { value: 'mine', label: 'My Links' },
    { value: 'all', label: IS_LNP ? 'All LNP Links' : 'All team links' },
];

interface LinkViewToggleProps {
    value: LinkView;
    onChange: (view: LinkView) => void;
}

/**
 * Segmented control, exposed as a radio group: one tab stop, arrow keys move
 * the choice. (Not a tablist: the create card owns the page's tabs.)
 */
export default function LinkViewToggle({ value, onChange }: LinkViewToggleProps) {
    const buttons = useRef<Record<LinkView, HTMLButtonElement | null>>({ mine: null, all: null });

    const onKeyDown = (event: React.KeyboardEvent) => {
        const step = event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1
            : event.key === 'ArrowLeft' || event.key === 'ArrowUp' ? -1
            : 0;
        if (step === 0) return;
        event.preventDefault();
        const index = OPTIONS.findIndex(option => option.value === value);
        const next = OPTIONS[(index + step + OPTIONS.length) % OPTIONS.length].value;
        onChange(next);
        buttons.current[next]?.focus();
    };

    return (
        <div
            role="radiogroup"
            aria-label="Links to show"
            onKeyDown={onKeyDown}
            className="inline-flex rounded-lg border border-line2 bg-surface p-0.5"
        >
            {OPTIONS.map(option => {
                const checked = option.value === value;
                return (
                    <button
                        key={option.value}
                        ref={(element) => { buttons.current[option.value] = element; }}
                        type="button"
                        role="radio"
                        aria-checked={checked}
                        tabIndex={checked ? 0 : -1}
                        onClick={() => { if (!checked) onChange(option.value); }}
                        className={`rounded-md px-3 py-1.5 text-sm font-medium transition-colors ${
                            checked ? 'bg-primary-600 text-white' : 'text-muted hover:text-ink'
                        }`}
                    >
                        {option.label}
                    </button>
                );
            })}
        </div>
    );
}
