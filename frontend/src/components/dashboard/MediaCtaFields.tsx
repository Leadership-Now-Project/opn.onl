import { CTA_LABEL_CHARS } from '../../utils/media';

interface MediaCtaFieldsProps {
    /** Prefix of the inputs' ids, so two forms on a page do not share them. */
    idPrefix: string;
    label: string;
    url: string;
    onLabelChange: (label: string) => void;
    onUrlChange: (url: string) => void;
    labelClass: string;
    fieldClass: string;
    disabled?: boolean;
}

/**
 * The optional button under a media page's player: what it says and where it
 * goes. Both or neither; `ctaProblem` says so before anything is sent.
 */
export default function MediaCtaFields({
    idPrefix, label, url, onLabelChange, onUrlChange, labelClass, fieldClass, disabled = false,
}: MediaCtaFieldsProps) {
    return (
        <fieldset>
            <legend className={labelClass}>Button under the video (optional)</legend>
            <div className="space-y-2">
                <label htmlFor={`${idPrefix}-cta-label`} className="sr-only">Button label</label>
                <input
                    id={`${idPrefix}-cta-label`}
                    type="text"
                    className={fieldClass}
                    placeholder="Label, e.g. Join us"
                    value={label}
                    maxLength={CTA_LABEL_CHARS}
                    disabled={disabled}
                    onChange={(e) => onLabelChange(e.target.value)}
                />
                <label htmlFor={`${idPrefix}-cta-url`} className="sr-only">Button link</label>
                <input
                    id={`${idPrefix}-cta-url`}
                    type="url"
                    inputMode="url"
                    className={fieldClass}
                    placeholder="Link, e.g. https://example.org/join"
                    value={url}
                    maxLength={2048}
                    disabled={disabled}
                    aria-describedby={`${idPrefix}-cta-help`}
                    onChange={(e) => onUrlChange(e.target.value)}
                />
            </div>
            <p id={`${idPrefix}-cta-help`} className="mt-1.5 text-xs text-faint">
                Shown under the player and opens in a new tab. Fill in both, or leave both empty.
            </p>
        </fieldset>
    );
}
