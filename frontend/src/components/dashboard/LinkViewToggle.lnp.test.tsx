import { describe, it, expect, vi } from 'vitest';

// The theme is a build-time switch, read when config/theme.ts is first evaluated.
vi.hoisted(() => {
    vi.stubEnv('VITE_THEME', 'lnp');
});

import { render, screen } from '../../test/test-utils';
import LinkViewToggle from './LinkViewToggle';

describe('LinkViewToggle under the LNP theme', () => {
    it('offers My Links and All LNP Links as one radio group', () => {
        render(<LinkViewToggle value="mine" onChange={() => {}} />);

        const group = screen.getByRole('radiogroup', { name: 'Links to show' });
        expect(group).toBeInTheDocument();
        const radios = screen.getAllByRole('radio');
        expect(radios.map(radio => radio.textContent)).toEqual(['My Links', 'All LNP Links']);
        expect(radios.map(radio => radio.getAttribute('aria-checked'))).toEqual(['true', 'false']);
        // One tab stop: the chosen one.
        expect(radios.map(radio => radio.tabIndex)).toEqual([0, -1]);
    });

    it('reports a click on the other option, and none on the chosen one', async () => {
        const onChange = vi.fn();
        const { user } = render(<LinkViewToggle value="mine" onChange={onChange} />);

        await user.click(screen.getByRole('radio', { name: 'My Links' }));
        expect(onChange).not.toHaveBeenCalled();
        await user.click(screen.getByRole('radio', { name: 'All LNP Links' }));
        expect(onChange).toHaveBeenCalledExactlyOnceWith('all');
    });
});
