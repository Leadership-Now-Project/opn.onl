import { act } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '../../test/test-utils';
import ProtectedDocument from './ProtectedDocument';

const MARK = 'pat@example.org · 2026-11-13 14:05 UTC';

function show() {
    return render(
        <div>
            <p data-testid="outside">outside the document</p>
            <ProtectedDocument watermark={MARK}>
                <p data-testid="page">a page</p>
            </ProtectedDocument>
        </div>,
    );
}

/** Dispatch a cancelable event and say whether the page let it through. */
function allowed(target: EventTarget, type: string, init: KeyboardEventInit = {}): boolean {
    const event = type === 'keydown'
        ? new KeyboardEvent(type, { bubbles: true, cancelable: true, ...init })
        : new Event(type, { bubbles: true, cancelable: true });
    return target.dispatchEvent(event);
}

function setVisibility(state: 'hidden' | 'visible') {
    Object.defineProperty(document, 'visibilityState', { value: state, configurable: true });
    act(() => { document.dispatchEvent(new Event('visibilitychange')); });
}

describe('ProtectedDocument', () => {
    it.each(['contextmenu', 'selectstart', 'dragstart', 'copy', 'cut'])(
        'refuses %s inside the document and leaves the rest of the page alone',
        (type) => {
            show();
            expect(allowed(screen.getByTestId('page'), type)).toBe(false);
            expect(allowed(screen.getByTestId('outside'), type)).toBe(true);
        },
    );

    it.each([
        ['Ctrl+P', { key: 'p', ctrlKey: true }],
        ['Cmd+P', { key: 'p', metaKey: true }],
        ['Ctrl+S', { key: 's', ctrlKey: true }],
        ['Cmd+S', { key: 'S', metaKey: true }],
    ])('swallows %s', (_name, init) => {
        show();
        expect(allowed(document.body, 'keydown', init)).toBe(false);
    });

    it('leaves other keys alone, and lets go of the keyboard when it is gone', () => {
        const { unmount } = show();
        expect(allowed(document.body, 'keydown', { key: 'p' })).toBe(true);
        expect(allowed(document.body, 'keydown', { key: 'f', ctrlKey: true })).toBe(true);
        expect(allowed(document.body, 'keydown', { key: 'ArrowDown' })).toBe(true);

        unmount();
        expect(allowed(document.body, 'keydown', { key: 'p', ctrlKey: true })).toBe(true);
    });

    it('cannot be selected and prints as nothing', () => {
        const { container } = show();
        const root = screen.getByTestId('protected-document');
        expect(root.style.userSelect).toBe('none');
        const print = container.querySelector('style')?.textContent ?? '';
        expect(print).toMatch(/@media print\s*\{\s*body\s*\{\s*display:\s*none/);
    });

    it('lays the watermark over the document, out of reach of the mouse', () => {
        show();
        const overlay = screen.getByTestId('watermark-overlay');
        expect(overlay).toHaveAttribute('data-watermark', MARK);
        expect(decodeURIComponent(overlay.style.backgroundImage)).toContain(MARK);
        expect(overlay).toHaveClass('pointer-events-none', 'absolute', 'inset-0');
        expect(overlay).toHaveAttribute('aria-hidden', 'true');
    });

    it('escapes whatever is in the watermark', () => {
        render(
            <ProtectedDocument watermark={'a<b>&"c@example.org'}>
                <p>page</p>
            </ProtectedDocument>,
        );
        const svg = decodeURIComponent(screen.getByTestId('watermark-overlay').style.backgroundImage);
        expect(svg).toContain('a&lt;b&gt;&amp;&quot;c@example.org');
        expect(svg).not.toContain('<b>');
    });

    it('blurs behind a veil when the window loses focus, and clears on focus', () => {
        show();
        const content = screen.getByTestId('document-content');
        expect(content).toHaveAttribute('data-veiled', 'false');
        expect(content.style.filter).toBe('');
        expect(screen.queryByRole('button', { name: 'Click to continue viewing' })).not.toBeInTheDocument();

        act(() => { window.dispatchEvent(new Event('blur')); });
        expect(content).toHaveAttribute('data-veiled', 'true');
        expect(content.style.filter).toContain('blur(');
        expect(screen.getByRole('button', { name: 'Click to continue viewing' })).toBeVisible();

        act(() => { window.dispatchEvent(new Event('focus')); });
        expect(content).toHaveAttribute('data-veiled', 'false');
        expect(content.style.filter).toBe('');
        expect(screen.queryByRole('button', { name: 'Click to continue viewing' })).not.toBeInTheDocument();
    });

    it('blurs when the tab is hidden, and a click on the veil brings the document back', () => {
        show();
        const content = screen.getByTestId('document-content');

        setVisibility('hidden');
        expect(content).toHaveAttribute('data-veiled', 'true');
        // Coming back into view is not enough by itself: the veil waits for
        // the window to be the one in front, or for a click.
        setVisibility('visible');
        expect(content).toHaveAttribute('data-veiled', 'true');

        fireEvent.click(screen.getByRole('button', { name: 'Click to continue viewing' }));
        expect(content).toHaveAttribute('data-veiled', 'false');
    });

    it('blurs before the page is printed', () => {
        show();
        act(() => { window.dispatchEvent(new Event('beforeprint')); });
        expect(screen.getByTestId('document-content')).toHaveAttribute('data-veiled', 'true');
    });
});
