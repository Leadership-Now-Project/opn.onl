import { shortLinkHost } from '../../config/api';
import { useAliasHint, type AliasSettings } from './aliasCheck';

interface AliasFieldProps {
    /** Prefix of the input's and the hints' ids, so two forms on a page do not share them. */
    idPrefix: string;
    alias: string;
    onChange: (alias: string) => void;
    settings: AliasSettings;
    labelClass: string;
    disabled?: boolean;
}

/**
 * "Short link": the host and the alias typed after it, with the live answer on
 * whether that alias is free. The Links and Files tabs both use it.
 */
export default function AliasField({ idPrefix, alias, onChange, settings, labelClass, disabled = false }: AliasFieldProps) {
    const aliasHint = useAliasHint(alias, settings);

    return (
        <>
            <p className={labelClass}>Short link</p>
            {/* Between `lg` and `xl` the card is too narrow for the
                host and a usable alias field side by side, so
                the field goes under the host. */}
            <div className="flex items-stretch lg:max-xl:flex-col">
                <span className="inline-flex items-center rounded-l-lg border border-r-0 border-line2 bg-paper px-3 font-mono text-sm text-muted lg:max-xl:rounded-bl-none lg:max-xl:rounded-tr-lg lg:max-xl:border-b-0 lg:max-xl:border-r lg:max-xl:py-2">
                    https://{shortLinkHost()}/
                </span>
                <label htmlFor={`${idPrefix}-alias`} className="sr-only">Custom alias</label>
                <input
                    id={`${idPrefix}-alias`}
                    type="text"
                    placeholder={`alias (${settings.min_alias_length}-${settings.max_alias_length} chars)`}
                    aria-describedby={aliasHint ? `${idPrefix}-alias-hint` : `${idPrefix}-alias-help`}
                    className="min-w-0 flex-1 rounded-r-lg border border-line2 bg-surface px-4 py-2.5 font-mono text-sm text-ink outline-none transition-colors focus:border-primary-500 placeholder:text-faint lg:max-xl:rounded-bl-lg lg:max-xl:rounded-tr-none"
                    value={alias}
                    onChange={(e) => onChange(e.target.value.replace(/[^a-zA-Z0-9_-]/g, ''))}
                    minLength={settings.min_alias_length}
                    maxLength={settings.max_alias_length}
                    disabled={disabled}
                />
            </div>
            {aliasHint ? (
                <p
                    id={`${idPrefix}-alias-hint`}
                    role="status"
                    className={`mt-1.5 text-xs ${aliasHint.ok ? 'text-success' : 'text-danger'}`}
                >
                    {aliasHint.text}
                </p>
            ) : (
                <p id={`${idPrefix}-alias-help`} className="mt-1.5 text-xs text-faint">
                    Leave empty for a random code.
                </p>
            )}
        </>
    );
}
