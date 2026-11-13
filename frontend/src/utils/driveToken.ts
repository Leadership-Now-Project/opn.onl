// The Google Drive access token, held in this module's memory and nowhere
// else. It lives apart from `googleDrive.ts` so that signing out can drop it
// (`forgetDriveToken`) without the public pages loading any of the Drive code:
// this file names no Google address and imports nothing.

export interface CachedDriveToken {
    clientId: string;
    token: string;
    expiresAt: number;
}

let cached: CachedDriveToken | null = null;

export function cachedDriveToken(): CachedDriveToken | null {
    return cached;
}

export function rememberDriveToken(token: CachedDriveToken): void {
    cached = token;
}

/**
 * Drop the token, so the next request asks Google again. Every way out of a
 * session calls this: the next person to sign in on the same tab must not
 * pick, or change the sharing of, files as the one before.
 */
export function forgetDriveToken(): void {
    cached = null;
}
