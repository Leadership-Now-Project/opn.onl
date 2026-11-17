import { useEffect, useState } from 'react';
import { API_ENDPOINTS, authFetch, responseError, type CheckCodeResponse } from '../../config/api';
import { aliasHintFrom, type AliasHint } from './linkFields';
import type { AppSettings } from './types';

export type AliasSettings = Pick<AppSettings, 'custom_aliases_enabled' | 'min_alias_length' | 'max_alias_length'>;

function isAbort(error: unknown): boolean {
    return typeof error === 'object' && error !== null && 'name' in error && (error as { name: unknown }).name === 'AbortError';
}

/**
 * Whether `alias` is free, asked of the server a moment after typing stops.
 * Below the configured minimum the server only repeats the length rule, so
 * nothing is asked until the alias could actually be free or taken.
 */
export function useAliasHint(alias: string, settings: AliasSettings): AliasHint | null {
    const [aliasHint, setAliasHint] = useState<AliasHint | null>(null);

    useEffect(() => {
        if (!settings.custom_aliases_enabled) return;
        const code = alias;
        if (code.length < settings.min_alias_length) {
            setAliasHint(null);
            return;
        }
        // Drop the previous answer immediately so a slow response cannot sit
        // under an alias it was not about.
        setAliasHint(null);
        const controller = new AbortController();
        let ignore = false;
        const timer = window.setTimeout(async () => {
            try {
                const res = await authFetch(`${API_ENDPOINTS.checkCode}?${new URLSearchParams({ code })}`, {
                    signal: controller.signal,
                });
                if (ignore) return;
                if (!res.ok) {
                    // A 429 here says how long to wait; anything else that
                    // failed is still worth a word rather than a silent blank.
                    const text = await responseError(res, 'Could not check this alias');
                    if (!ignore) setAliasHint({ text, ok: false });
                    return;
                }
                const data = await res.json() as CheckCodeResponse;
                if (ignore || data.code !== code) return;
                setAliasHint(aliasHintFrom(data));
            } catch (error) {
                if (ignore || isAbort(error)) return;
                setAliasHint(null);
            }
        }, 300);
        return () => {
            ignore = true;
            window.clearTimeout(timer);
            controller.abort();
        };
    }, [alias, settings.custom_aliases_enabled, settings.min_alias_length]);

    return aliasHint;
}

/** The message for an alias the form should not send, or null when it may. */
export function aliasProblem(alias: string, settings: AliasSettings): string | null {
    if (!alias) return null;
    if (alias.length < settings.min_alias_length) {
        return `Alias must be at least ${settings.min_alias_length} characters`;
    }
    if (alias.length > settings.max_alias_length) {
        return `Alias must be at most ${settings.max_alias_length} characters`;
    }
    if (!/^[a-zA-Z0-9_-]+$/.test(alias)) {
        return 'Alias can only contain letters, numbers, hyphens, and underscores';
    }
    if (/^[-_]|[-_]$/.test(alias)) {
        return 'Alias cannot start or end with hyphen or underscore';
    }
    return null;
}
