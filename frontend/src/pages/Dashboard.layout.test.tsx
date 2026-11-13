import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, within } from '../test/test-utils';
import Dashboard from './Dashboard';
import { mockFetchResponse, mockToken } from '../test/test-utils';

// The create card's tabs and the "Shared Links" area. jsdom has no layout, so
// the two-column grid itself is not asserted here.
describe('Dashboard layout', () => {
  beforeEach(() => {
    vi.mocked(global.fetch).mockReset();
    vi.mocked(localStorage.getItem).mockReturnValue(mockToken);
    vi.mocked(global.fetch).mockImplementation(() => mockFetchResponse([]) as any);
  });

  const ready = async () => {
    const view = render(<Dashboard />);
    await waitFor(() => {
      expect(screen.getByRole('tablist')).toBeInTheDocument();
    });
    return view;
  };

  it('offers Links, Files and Media tabs in the create card', async () => {
    await ready();

    const tabs = within(screen.getByRole('tablist')).getAllByRole('tab');
    expect(tabs.map(tab => tab.textContent)).toEqual(['Links', 'Files', 'Media']);
    expect(screen.getByRole('tablist')).toHaveAccessibleName('Create new link');
  });

  it('opens on the Links tab with the create form in its panel', async () => {
    await ready();

    expect(screen.getByRole('tab', { name: 'Links' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tab', { name: 'Files' })).toHaveAttribute('aria-selected', 'false');
    expect(screen.getByRole('tab', { name: 'Media' })).toHaveAttribute('aria-selected', 'false');

    const panel = screen.getByRole('tabpanel');
    expect(panel).toHaveAccessibleName('Links');
    expect(within(panel).getByPlaceholderText(/example.com/i)).toBeVisible();
    expect(within(panel).getByRole('button', { name: 'Paste' })).toBeVisible();
    expect(within(panel).getByRole('button', { name: 'Check destination' })).toBeVisible();
    expect(within(panel).getByRole('button', { name: 'Create' })).toBeVisible();
    expect(within(panel).getByRole('button', { name: 'Advanced options' })).toBeVisible();
    // The other panels stay in the document, hidden.
    for (const placeholder of screen.getAllByText('Coming soon.')) {
      expect(placeholder).not.toBeVisible();
    }
  });

  it('shows the Files placeholder and hides the form on the Files tab', async () => {
    const { user } = await ready();

    await user.click(screen.getByRole('tab', { name: 'Files' }));

    expect(screen.getByRole('tab', { name: 'Files' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tab', { name: 'Links' })).toHaveAttribute('aria-selected', 'false');
    const panel = screen.getByRole('tabpanel');
    expect(panel).toHaveAccessibleName('Files');
    expect(within(panel).getByText('Coming soon.')).toBeVisible();
    expect(
      within(panel).getByText('Share a file from Google Drive, OneDrive or your computer.'),
    ).toBeVisible();
    expect(screen.getByPlaceholderText(/example.com/i)).not.toBeVisible();
    expect(screen.queryByRole('button', { name: 'Create' })).not.toBeInTheDocument();
  });

  it('shows the Media placeholder and hides the form on the Media tab', async () => {
    const { user } = await ready();

    await user.click(screen.getByRole('tab', { name: 'Media' }));

    expect(screen.getByRole('tab', { name: 'Media' })).toHaveAttribute('aria-selected', 'true');
    const panel = screen.getByRole('tabpanel');
    expect(panel).toHaveAccessibleName('Media');
    expect(within(panel).getByText('Coming soon.')).toBeVisible();
    expect(
      within(panel).getByText('Share a video or audio clip on an LNP-branded page.'),
    ).toBeVisible();
    expect(screen.getByPlaceholderText(/example.com/i)).not.toBeVisible();
    expect(screen.queryByRole('button', { name: 'Create' })).not.toBeInTheDocument();
  });

  it('keeps what was typed in the form across a visit to another tab', async () => {
    const { user } = await ready();

    await user.type(screen.getByPlaceholderText(/example.com/i), 'https://test.com/kept');
    await user.click(screen.getByRole('tab', { name: 'Files' }));
    await user.click(screen.getByRole('tab', { name: 'Links' }));

    expect(screen.getByPlaceholderText(/example.com/i)).toBeVisible();
    expect(screen.getByPlaceholderText(/example.com/i)).toHaveValue('https://test.com/kept');
  });

  it('moves between tabs with the arrow, Home and End keys', async () => {
    const { user } = await ready();

    screen.getByRole('tab', { name: 'Links' }).focus();
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Files' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tab', { name: 'Files' })).toHaveFocus();

    await user.keyboard('{End}');
    expect(screen.getByRole('tab', { name: 'Media' })).toHaveAttribute('aria-selected', 'true');
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Links' })).toHaveAttribute('aria-selected', 'true');
    await user.keyboard('{ArrowLeft}');
    expect(screen.getByRole('tab', { name: 'Media' })).toHaveFocus();
    await user.keyboard('{Home}');
    expect(screen.getByRole('tab', { name: 'Links' })).toHaveFocus();

    // Only the chosen tab is a Tab stop.
    expect(screen.getByRole('tab', { name: 'Links' })).toHaveAttribute('tabindex', '0');
    expect(screen.getByRole('tab', { name: 'Files' })).toHaveAttribute('tabindex', '-1');
  });

  it('heads the list area "Shared Links", with search and filters inside it', async () => {
    await ready();

    expect(screen.getByRole('heading', { name: 'Shared Links', level: 2 })).toBeInTheDocument();
    const region = screen.getByRole('region', { name: 'Shared Links' });
    expect(within(region).getByRole('searchbox', { name: 'Search links' })).toBeInTheDocument();
    expect(within(region).getByRole('combobox', { name: 'Sort links by' })).toBeInTheDocument();
    expect(within(region).getByRole('combobox', { name: 'Filter by folder' })).toBeInTheDocument();
    expect(within(region).queryByRole('tablist')).not.toBeInTheDocument();
  });

  it('"Start shortening" returns to the Links tab and focuses the destination', async () => {
    const { user } = await ready();

    await user.click(screen.getByRole('tab', { name: 'Media' }));
    await user.click(screen.getByRole('button', { name: /start shortening/i }));

    expect(screen.getByRole('tab', { name: 'Links' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByPlaceholderText(/example.com/i)).toHaveFocus();
  });
});
