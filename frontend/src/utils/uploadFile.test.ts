import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { putFile, UploadCancelled } from './uploadFile';

// jsdom's XMLHttpRequest would try to reach the network. This stand-in records
// what putFile asks of it and lets a test play the browser's side.
class FakeRequest {
    static last: FakeRequest;
    method = '';
    url = '';
    headers: Record<string, string> = {};
    body: unknown = null;
    status = 0;
    aborted = false;
    upload: { onprogress: ((event: unknown) => void) | null } = { onprogress: null };
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    onabort: (() => void) | null = null;

    constructor() {
        FakeRequest.last = this;
    }
    open(method: string, url: string) {
        this.method = method;
        this.url = url;
    }
    setRequestHeader(name: string, value: string) {
        this.headers[name] = value;
    }
    send(body: unknown) {
        this.body = body;
    }
    abort() {
        this.aborted = true;
        this.onabort?.();
    }
}

const URL = 'https://bucket.example.test/files/abc?X-Amz-Signature=s';
const file = new Blob(['hello'], { type: 'application/pdf' });

describe('putFile', () => {
    beforeEach(() => {
        vi.stubGlobal('XMLHttpRequest', FakeRequest);
    });
    afterEach(() => {
        vi.unstubAllGlobals();
    });

    it('PUTs the file to the signed URL with exactly the headers it was given', async () => {
        const done = putFile(URL, { 'content-type': 'application/pdf' }, file, vi.fn(), new AbortController().signal);
        const request = FakeRequest.last;

        expect(request.method).toBe('PUT');
        expect(request.url).toBe(URL);
        // No session token goes to storage: only what the URL was signed with.
        expect(request.headers).toEqual({ 'content-type': 'application/pdf' });
        expect(request.body).toBe(file);

        request.status = 200;
        request.onload?.();
        await expect(done).resolves.toBeUndefined();
    });

    it('reports progress as a fraction and ends on 1', async () => {
        const onProgress = vi.fn();
        const done = putFile(URL, {}, file, onProgress, new AbortController().signal);
        const request = FakeRequest.last;

        request.upload.onprogress?.({ lengthComputable: true, loaded: 25, total: 100 });
        request.upload.onprogress?.({ lengthComputable: false, loaded: 50, total: 0 });
        request.status = 204;
        request.onload?.();
        await done;

        expect(onProgress.mock.calls.map(call => call[0])).toEqual([0.25, 1]);
    });

    it('fails when storage refuses the upload', async () => {
        const done = putFile(URL, {}, file, vi.fn(), new AbortController().signal);
        FakeRequest.last.status = 403;
        FakeRequest.last.onload?.();

        await expect(done).rejects.toThrow('Storage refused the upload (403)');
    });

    it('fails when the upload never reaches storage', async () => {
        const done = putFile(URL, {}, file, vi.fn(), new AbortController().signal);
        FakeRequest.last.onerror?.();

        await expect(done).rejects.toThrow('The upload did not reach storage');
    });

    it('stops the request when the signal is aborted', async () => {
        const controller = new AbortController();
        const done = putFile(URL, {}, file, vi.fn(), controller.signal);
        const request = FakeRequest.last;

        controller.abort();

        await expect(done).rejects.toBeInstanceOf(UploadCancelled);
        expect(request.aborted).toBe(true);
    });

    it('sends nothing when the signal was aborted beforehand', async () => {
        const controller = new AbortController();
        controller.abort();
        const before = FakeRequest.last;

        await expect(putFile(URL, {}, file, vi.fn(), controller.signal)).rejects.toBeInstanceOf(UploadCancelled);
        expect(FakeRequest.last).toBe(before);
    });
});
