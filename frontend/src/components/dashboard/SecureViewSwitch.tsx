import { FileLock2 } from 'lucide-react';
import { SECURE_VIEW_EXPLANATION } from '../../utils/secureView';

interface SecureViewSwitchProps {
    id: string;
    checked: boolean;
    onChange: (checked: boolean) => void;
    disabled?: boolean;
    /** Why it is greyed out. Shown in place of the explanation. */
    reason?: string;
}

/**
 * The "Secure View Share" switch, as the Files tab and the edit dialog show
 * it: off unless someone turns it on, and greyed out with the reason when the
 * file is one the viewer cannot show.
 */
export default function SecureViewSwitch({ id, checked, onChange, disabled = false, reason = '' }: SecureViewSwitchProps) {
    const helpId = `${id}-help`;
    return (
        <div className={`mt-3 ${disabled ? 'opacity-60' : ''}`}>
            <label
                htmlFor={id}
                className={`flex items-center gap-2.5 text-sm text-ink ${disabled ? 'cursor-not-allowed' : 'cursor-pointer'}`}
            >
                <input
                    id={id}
                    type="checkbox"
                    role="switch"
                    checked={checked}
                    disabled={disabled}
                    aria-describedby={helpId}
                    onChange={(e) => onChange(e.target.checked)}
                    className="h-4 w-4 rounded border-line2 text-primary-600 focus:ring-primary-500"
                />
                <span className="inline-flex items-center gap-1.5 font-medium">
                    <FileLock2 className="h-3.5 w-3.5 text-primary-600" aria-hidden="true" />
                    Secure View Share
                </span>
            </label>
            <p id={helpId} className="mt-1.5 text-xs text-faint">
                {reason || SECURE_VIEW_EXPLANATION}
            </p>
        </div>
    );
}
