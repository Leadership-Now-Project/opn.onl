/**
 * Secure View: a file link that opens a viewer (email first, then the file
 * shown with deterrents) instead of handing the file over.
 *
 * The server decides which files it can be switched on for
 * (`utils/secure_view.rs`, `eligible`, built on `files::served_as`). This is
 * the same rule, run when a file is chosen so the switch can be greyed out
 * before anything is uploaded. The two change together.
 */

/** The types a browser shows in place, each with the extensions that go with it. */
const VIEWABLE_TYPES: Record<string, string[]> = {
    'application/pdf': ['pdf'],
    'image/png': ['png'],
    'image/jpeg': ['jpg', 'jpeg'],
    'image/gif': ['gif'],
    'image/webp': ['webp'],
};

/** One line under the switch, wherever it appears. */
export const SECURE_VIEW_EXPLANATION =
    'Viewers must enter an email and see the file in a protected viewer. It deters saving and printing; it cannot stop screenshots.';

/** Why the switch is greyed out for a file it cannot show. */
export const SECURE_VIEW_NOT_ELIGIBLE = 'Only for PDF, PNG, JPEG, GIF and WebP files.';

/**
 * Whether Secure View can show a file: its declared type is one of the five,
 * and the last extension of its name belongs to that type.
 */
export function secureViewEligible(filename: string, contentType: string): boolean {
    const declared = contentType.split(';')[0].trim().toLowerCase();
    const dot = filename.lastIndexOf('.');
    const extension = dot < 0 ? '' : filename.slice(dot + 1).trim().toLowerCase();
    return (VIEWABLE_TYPES[declared] ?? []).includes(extension);
}
