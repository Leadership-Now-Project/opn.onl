import { safeLocalStorage } from '../../utils/storage';

/** Whose links the shared list shows: the viewer's own, or everyone's in the organization. */
export type LinkView = 'mine' | 'all';

const STORAGE_KEY = 'dashboard.linkView';

/** The view chosen last time in this browser. "My Links" until one is chosen. */
export function readLinkView(): LinkView {
    return safeLocalStorage.getItem(STORAGE_KEY) === 'all' ? 'all' : 'mine';
}

export function rememberLinkView(view: LinkView): void {
    safeLocalStorage.setItem(STORAGE_KEY, view);
}
