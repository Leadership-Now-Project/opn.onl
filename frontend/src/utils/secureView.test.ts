import { describe, expect, it } from 'vitest';
import { secureViewEligible } from './secureView';

// The same cases as the server's rule (`files::served_as`, which
// `secure_view::eligible` is built on): the two must give the same answers.
describe('secureViewEligible', () => {
    it.each([
        ['report.pdf', 'application/pdf'],
        ['a.png', 'image/png'],
        ['a.jpg', 'image/jpeg'],
        ['a.jpeg', 'image/jpeg'],
        ['a.gif', 'image/gif'],
        ['a.webp', 'image/webp'],
        ['REPORT.PDF', 'Application/PDF'],
        ['a.png', 'image/png; charset=binary'],
        ['page.html.png', 'image/png'],
    ])('%s as %s can be shown', (name, type) => {
        expect(secureViewEligible(name, type)).toBe(true);
    });

    it.each([
        ['bundle.zip', 'application/zip'],
        ['notes.docx', 'application/vnd.openxmlformats-officedocument.wordprocessingml.document'],
        ['page.html', 'text/html'],
        ['logo.svg', 'image/svg+xml'],
        ['clip.mp4', 'video/mp4'],
        // The name and the declared type must agree.
        ['page.html', 'application/pdf'],
        ['report.pdf', 'text/html'],
        ['a.jpg', 'image/png'],
        ['report.pdf.html', 'application/pdf'],
        // No type from the browser, or no extension.
        ['report.pdf', ''],
        ['report.pdf', 'application/octet-stream'],
        ['report', 'application/pdf'],
        ['photo.', 'image/png'],
        ['', 'image/png'],
    ])('%s as %s cannot', (name, type) => {
        expect(secureViewEligible(name, type)).toBe(false);
    });
});
