/** Thrown by `putFile` when the caller's signal stopped the upload. */
export class UploadCancelled extends Error {
    constructor() {
        super('Upload cancelled');
        this.name = 'UploadCancelled';
    }
}

/**
 * Send `file` to a signed storage URL with `PUT`, reporting progress as a
 * fraction from 0 to 1. The bytes go straight from the browser to storage:
 * this request carries no session token, only the headers the URL was signed
 * with. `XMLHttpRequest` because `fetch` cannot report upload progress.
 */
export function putFile(
    url: string,
    headers: Record<string, string>,
    file: Blob,
    onProgress: (fraction: number) => void,
    signal: AbortSignal,
): Promise<void> {
    return new Promise((resolve, reject) => {
        if (signal.aborted) {
            reject(new UploadCancelled());
            return;
        }
        const request = new XMLHttpRequest();
        const abort = () => request.abort();
        const done = () => signal.removeEventListener('abort', abort);

        request.open('PUT', url);
        for (const [name, value] of Object.entries(headers)) {
            request.setRequestHeader(name, value);
        }
        request.upload.onprogress = (event) => {
            if (event.lengthComputable && event.total > 0) onProgress(event.loaded / event.total);
        };
        request.onload = () => {
            done();
            if (request.status >= 200 && request.status < 300) {
                onProgress(1);
                resolve();
            } else {
                reject(new Error(`Storage refused the upload (${request.status})`));
            }
        };
        request.onerror = () => {
            done();
            reject(new Error('The upload did not reach storage'));
        };
        request.onabort = () => {
            done();
            reject(new UploadCancelled());
        };
        signal.addEventListener('abort', abort);
        request.send(file);
    });
}
