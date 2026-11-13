import { afterEach, describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '../test/test-utils';
import Login from './Login';
import { mockFetchResponse, mockFetchError, mockToken } from '../test/test-utils';
import { blockSiteStorage } from '../test/helpers';

describe('Login Page', () => {
  beforeEach(() => {
    vi.mocked(global.fetch).mockReset();
    vi.mocked(localStorage.getItem).mockReturnValue(null);
    vi.mocked(localStorage.setItem).mockClear();
  });

  afterEach(() => {
    Object.defineProperty(window, 'PublicKeyCredential', {
      configurable: true,
      value: undefined,
    });
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: undefined,
    });
  });

  it('renders login form', () => {
    render(<Login />);
    expect(screen.getByText('Welcome back')).toBeInTheDocument();
    expect(screen.getByLabelText(/email address/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/password/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /sign in/i })).toBeInTheDocument();
  });

  it('posts the form so a native submit cannot leak credentials into the URL', () => {
    render(<Login />);
    expect(screen.getByLabelText(/email address/i).closest('form')).toHaveAttribute('method', 'post');
  });

  it('shows link to register page', () => {
    render(<Login />);
    expect(screen.getByRole('link', { name: /create an account/i })).toBeInTheDocument();
  });

  it('validates email input', async () => {
    const { user } = render(<Login />);
    
    const emailInput = screen.getByLabelText(/email address/i);
    await user.type(emailInput, 'invalid-email');
    
    expect(emailInput).toHaveValue('invalid-email');
  });

  it('submits form with credentials', async () => {
    vi.mocked(global.fetch).mockResolvedValue(
      mockFetchResponse({ token: mockToken }) as any
    );

    const { user } = render(<Login />);
    
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.type(screen.getByLabelText(/password/i), 'password123');
    await user.click(screen.getByRole('button', { name: /sign in/i }));

    await waitFor(() => {
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining('/auth/login'),
        expect.objectContaining({
          method: 'POST',
          body: JSON.stringify({ email: 'test@example.com', password: 'password123' }),
        })
      );
    });
  });

  it('stores token on successful login', async () => {
    vi.mocked(global.fetch).mockResolvedValue(
      mockFetchResponse({ token: mockToken }) as any
    );

    const { user } = render(<Login />);
    
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.type(screen.getByLabelText(/password/i), 'password123');
    await user.click(screen.getByRole('button', { name: /sign in/i }));

    await waitFor(() => {
      expect(localStorage.setItem).toHaveBeenCalledWith('token', mockToken);
    });
  });

  describe('Google sign-in', () => {
    const CLIENT_ID = 'client-1.apps.googleusercontent.com';
    const googleButton = () => screen.queryByRole('button', { name: /sign in with google/i });
    const gsiScripts = () => document.head.querySelectorAll('script[src*="accounts.google.com/gsi/client"]');

    // What Google's script would put on window, with a popup that answers
    // through whichever callback the test picks.
    let requestCode: ReturnType<typeof vi.fn>;
    let popupAnswers: (config: {
      callback: (r: { code?: string; error?: string }) => void;
      error_callback?: (e: { type?: string }) => void;
    }) => void;
    let initCodeClient: ReturnType<typeof vi.fn>;

    // Every request that is not /auth/settings, i.e. every sign-in attempt.
    const signInCalls = () =>
      vi.mocked(global.fetch).mock.calls.filter(([url]) => !String(url).includes('/auth/settings'));

    const serve = (settings: unknown, google: () => unknown = () => mockFetchResponse({})) =>
      vi.mocked(global.fetch).mockImplementation(((url: string) =>
        String(url).includes('/auth/google') ? google() : mockFetchResponse(settings)) as any);

    beforeEach(() => {
      popupAnswers = (config) => config.callback({ code: '4/the-code' });
      requestCode = vi.fn();
      initCodeClient = vi.fn((config) => {
        requestCode.mockImplementation(() => popupAnswers(config));
        return { requestCode };
      });
      window.google = { accounts: { oauth2: { initCodeClient } } } as unknown as typeof window.google;
    });

    afterEach(() => {
      delete window.google;
      gsiScripts().forEach((s) => s.remove());
    });

    it('is not offered, and nothing is loaded from Google, when the instance has no client id', async () => {
      delete window.google;
      serve({ passkeys_enabled: true });
      render(<Login />);

      await waitFor(() => {
        expect(global.fetch).toHaveBeenCalledWith(expect.stringContaining('/auth/settings'));
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(googleButton()).not.toBeInTheDocument();
      expect(screen.getByLabelText(/password/i)).toBeInTheDocument();
      expect(gsiScripts()).toHaveLength(0);
    });

    it('replaces the password form, its links and the passkey block when the instance has a client id', async () => {
      Object.defineProperty(window, 'PublicKeyCredential', {
        configurable: true,
        value: class PublicKeyCredentialMock {},
      });
      serve({ passkeys_enabled: true, google_client_id: CLIENT_ID });
      render(<Login />);

      expect(await screen.findByRole('button', { name: /sign in with google/i })).toBeInTheDocument();
      expect(screen.queryByLabelText(/email address/i)).not.toBeInTheDocument();
      expect(screen.queryByLabelText(/password/i)).not.toBeInTheDocument();
      expect(screen.queryByRole('link', { name: /forgot/i })).not.toBeInTheDocument();
      expect(screen.queryByRole('link', { name: /create an account/i })).not.toBeInTheDocument();
      expect(screen.queryByRole('button', { name: /sign in with passkey/i })).not.toBeInTheDocument();
      expect(screen.queryByRole('button', { name: /^sign in$/i })).not.toBeInTheDocument();
    });

    it("loads Google's script only once the settings name a client id", async () => {
      delete window.google;
      serve({ google_client_id: CLIENT_ID });
      expect(gsiScripts()).toHaveLength(0);
      render(<Login />);

      await screen.findByRole('button', { name: /sign in with google/i });
      await waitFor(() => expect(gsiScripts()).toHaveLength(1));
    });

    it('signs nobody in on page load, with or without a client id', async () => {
      serve({ google_client_id: CLIENT_ID }, () => mockFetchResponse({ token: mockToken, is_admin: true }));
      render(<Login />);

      await screen.findByRole('button', { name: /sign in with google/i });
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(signInCalls()).toHaveLength(0);
      expect(initCodeClient).not.toHaveBeenCalled();
      expect(localStorage.setItem).not.toHaveBeenCalledWith('token', expect.anything());
      expect(window.location.pathname).not.toBe('/dashboard');
    });

    it('posts the code from the popup, stores the session and opens the dashboard', async () => {
      serve({ google_client_id: CLIENT_ID }, () =>
        mockFetchResponse({ token: mockToken, email_verified: true, is_admin: true }));
      const { user } = render(<Login />, { route: '/login' });

      await user.click(await screen.findByRole('button', { name: /sign in with google/i }));

      await waitFor(() => {
        expect(window.location.pathname).toBe('/dashboard');
      });
      expect(initCodeClient).toHaveBeenCalledWith(expect.objectContaining({
        client_id: CLIENT_ID,
        scope: 'openid email profile',
        ux_mode: 'popup',
      }));
      expect(requestCode).toHaveBeenCalledTimes(1);
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining('/auth/google'),
        expect.objectContaining({
          method: 'POST',
          body: JSON.stringify({ code: '4/the-code' }),
          headers: expect.objectContaining({ 'X-Requested-With': 'XmlHttpRequest' }),
        }),
      );
      expect(localStorage.setItem).toHaveBeenCalledWith('token', mockToken);
      expect(localStorage.setItem).toHaveBeenCalledWith('is_admin', 'true');
    });

    it('tells a refused account to use its Leadership Now Project account', async () => {
      serve({ google_client_id: CLIENT_ID }, () =>
        mockFetchError('This Google account is not allowed', 403));
      const { user } = render(<Login />, { route: '/login' });

      await user.click(await screen.findByRole('button', { name: /sign in with google/i }));

      expect(await screen.findByRole('alert')).toHaveTextContent(
        'This Google account is not allowed. Use your Leadership Now Project account.',
      );
      expect(localStorage.setItem).not.toHaveBeenCalledWith('token', expect.anything());
      expect(window.location.pathname).toBe('/login');
    });

    it("keeps the server's words for a disabled account", async () => {
      serve({ google_client_id: CLIENT_ID }, () => mockFetchError('This account is disabled', 403));
      const { user } = render(<Login />);

      await user.click(await screen.findByRole('button', { name: /sign in with google/i }));

      expect(await screen.findByRole('alert')).toHaveTextContent('This account is disabled');
    });

    it('says so when the Google window is closed, and sends nothing to the server', async () => {
      popupAnswers = (config) => config.error_callback?.({ type: 'popup_closed' });
      serve({ google_client_id: CLIENT_ID });
      const { user } = render(<Login />);

      const button = await screen.findByRole('button', { name: /sign in with google/i });
      await user.click(button);

      expect(await screen.findByRole('alert')).toHaveTextContent(/window was closed/i);
      expect(signInCalls()).toHaveLength(0);
      // Ready for another try.
      expect(button).toBeEnabled();
    });

    it('shows a plain failure when the server cannot complete the sign-in', async () => {
      serve({ google_client_id: CLIENT_ID }, () => mockFetchError('Could not reach Google; try again', 502));
      const { user } = render(<Login />);

      await user.click(await screen.findByRole('button', { name: /sign in with google/i }));

      expect(await screen.findByRole('alert')).toHaveTextContent('Could not reach Google; try again');
    });
  });

  it('stores admin metadata on successful passkey login', async () => {
    Object.defineProperty(window, 'PublicKeyCredential', {
      configurable: true,
      value: class PublicKeyCredentialMock {},
    });
    const credential = {
      id: 'credential-id',
      rawId: new Uint8Array([1, 2, 3]).buffer,
      type: 'public-key',
      response: {
        authenticatorData: new Uint8Array([4]).buffer,
        clientDataJSON: new Uint8Array([5]).buffer,
        signature: new Uint8Array([6]).buffer,
        userHandle: null,
      },
    } as unknown as PublicKeyCredential;
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: {
        get: vi.fn().mockResolvedValue(credential),
      },
    });
    vi.mocked(global.fetch).mockImplementation(((url: string) => {
      if (url.includes('/passkey/login/start')) {
        return mockFetchResponse({
          options: {
            publicKey: {
              challenge: 'AQID',
              allowCredentials: [{ id: 'BAUG', type: 'public-key' }],
            },
          },
          login_id: 'attempt-1',
        });
      }
      if (url.includes('/passkey/login/finish')) {
        return mockFetchResponse({ token: mockToken, email_verified: true, is_admin: true });
      }
      return mockFetchResponse({ passkeys_enabled: true });
    }) as any);

    const { user } = render(<Login />);
    await user.type(screen.getByLabelText(/email address/i), 'admin@example.com');
    await user.click(screen.getByRole('button', { name: /sign in with passkey/i }));

    await waitFor(() => {
      expect(localStorage.setItem).toHaveBeenCalledWith('token', mockToken);
      expect(localStorage.setItem).toHaveBeenCalledWith('is_admin', 'true');
    });

    // The server keys the pending challenge by the id it handed out at start,
    // not by the email, so finish must send that id back.
    const finish = vi
      .mocked(global.fetch)
      .mock.calls.find(([url]) => String(url).includes('/passkey/login/finish'));
    expect(finish).toBeDefined();
    const sent = JSON.parse(String((finish![1] as RequestInit).body));
    expect(sent.login_id).toBe('attempt-1');
    expect(sent.credential.id).toBe('credential-id');
  });

  describe('passkey button', () => {
    const passkeyButton = () => screen.queryByRole('button', { name: /sign in with passkey/i });

    beforeEach(() => {
      Object.defineProperty(window, 'PublicKeyCredential', {
        configurable: true,
        value: class PublicKeyCredentialMock {},
      });
    });

    const settingsAnswer = (body: unknown) =>
      vi.mocked(global.fetch).mockImplementation((() => mockFetchResponse(body)) as any);

    it('is shown on an instance with passkeys enabled', async () => {
      settingsAnswer({ passkeys_enabled: true });
      render(<Login />);

      await waitFor(() => {
        expect(global.fetch).toHaveBeenCalledWith(expect.stringContaining('/auth/settings'));
      });
      expect(passkeyButton()).toBeInTheDocument();
    });

    it('is hidden, with its hint, once the instance reports passkeys disabled', async () => {
      settingsAnswer({ passkeys_enabled: false });
      render(<Login />);

      await waitFor(() => {
        expect(passkeyButton()).not.toBeInTheDocument();
      });
      expect(screen.queryByText(/use your passkey/i)).not.toBeInTheDocument();
      // The password form is all that is left, and it still works.
      expect(screen.getByRole('button', { name: /^sign in$/i })).toBeInTheDocument();
    });

    it('stays when the settings cannot be read, since the password form does not depend on them', async () => {
      vi.mocked(global.fetch).mockRejectedValue(new Error('offline'));
      render(<Login />);

      await waitFor(() => {
        expect(global.fetch).toHaveBeenCalledWith(expect.stringContaining('/auth/settings'));
      });
      expect(passkeyButton()).toBeInTheDocument();
    });
  });

  it('says why instead of bouncing back to the form when the browser blocks site storage', async () => {
    vi.mocked(global.fetch).mockResolvedValue(
      mockFetchResponse({ token: mockToken, email_verified: true }) as any
    );
    const unblock = blockSiteStorage();
    try {
      const { user } = render(<Login />);

      await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
      await user.type(screen.getByLabelText(/password/i), 'password123');
      await user.click(screen.getByRole('button', { name: /sign in/i }));

      expect(await screen.findByText(/blocking storage for this site/i)).toBeInTheDocument();
      // Without a stored token the dashboard would only send the visitor back here.
      expect(window.location.pathname).toBe('/');
    } finally {
      unblock();
    }
  });

  it('asks an unverified account to verify when the instance requires it', async () => {
    vi.mocked(global.fetch).mockImplementation(((url: string) => {
      if (url.includes('/auth/login')) {
        return mockFetchResponse({ token: mockToken, email_verified: false });
      }
      return mockFetchResponse({ require_email_verification: true, passkeys_enabled: true });
    }) as any);

    const { user } = render(<Login />, { route: '/login' });
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.type(screen.getByLabelText(/password/i), 'password123');
    await user.click(screen.getByRole('button', { name: /^sign in$/i }));

    expect(await screen.findByRole('heading', { name: 'Verify your email' })).toBeInTheDocument();
    expect(window.location.pathname).toBe('/login');
    expect(global.fetch).toHaveBeenCalledWith(expect.stringContaining('/auth/settings'));
  });

  it('sends an unverified account to the dashboard when verification is not required', async () => {
    vi.mocked(global.fetch).mockImplementation(((url: string) => {
      if (url.includes('/auth/login')) {
        return mockFetchResponse({ token: mockToken, email_verified: false });
      }
      return mockFetchResponse({ require_email_verification: false, passkeys_enabled: true });
    }) as any);

    const { user } = render(<Login />, { route: '/login' });
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.type(screen.getByLabelText(/password/i), 'password123');
    await user.click(screen.getByRole('button', { name: /^sign in$/i }));

    await waitFor(() => {
      expect(window.location.pathname).toBe('/dashboard');
    });
    expect(screen.queryByRole('heading', { name: 'Verify your email' })).not.toBeInTheDocument();
  });

  it('shows the server error when a verification email cannot be sent', async () => {
    vi.mocked(global.fetch).mockImplementation(((url: string) => {
      if (url.includes('/auth/resend-verification')) {
        return mockFetchResponse({ error: 'Email is not configured on this instance' }, 503);
      }
      if (url.includes('/auth/login')) {
        return mockFetchResponse({ token: mockToken, email_verified: false });
      }
      return mockFetchResponse({ require_email_verification: true, passkeys_enabled: true });
    }) as any);

    const { user } = render(<Login />, { route: '/login' });
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.type(screen.getByLabelText(/password/i), 'password123');
    await user.click(screen.getByRole('button', { name: /^sign in$/i }));
    const resend = await screen.findByRole('button', { name: /resend verification email/i });
    // Sign-in stored the token; this mock's storage returns it for the resend only,
    // because a token present at mount sends the page to the dashboard.
    vi.mocked(localStorage.getItem).mockImplementationOnce((key: string) => (key === 'token' ? mockToken : null));
    await user.click(resend);

    expect(await screen.findByRole('alert')).toHaveTextContent('Email is not configured on this instance');
    expect(screen.queryByText('Email sent')).not.toBeInTheDocument();
    expect(global.fetch).toHaveBeenCalledWith(
      expect.stringContaining('/auth/resend-verification'),
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ email: 'test@example.com' }),
        // Signed in, so the server answers for this account, not anonymously.
        headers: expect.objectContaining({ Authorization: `Bearer ${mockToken}` }),
      }),
    );
  });

  it('shows error on failed login', async () => {
    vi.mocked(global.fetch).mockResolvedValue(
      mockFetchError('Invalid credentials', 401) as any
    );

    const { user } = render(<Login />);
    
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.type(screen.getByLabelText(/password/i), 'wrongpassword');
    await user.click(screen.getByRole('button', { name: /sign in/i }));

    await waitFor(() => {
      expect(screen.getByText(/invalid credentials/i)).toBeInTheDocument();
    });
  });

  // The middleware's 429: `error` names the limit, `message` says how long to wait.
  const rateLimited = () =>
    mockFetchResponse(
      {
        error: 'Too many requests',
        retry_after: 37,
        message: 'Rate limit exceeded. Please try again in 37 seconds.',
      },
      429,
    );

  it('says how long to wait when sign-in is rate limited', async () => {
    vi.mocked(global.fetch).mockImplementation(((url: string) =>
      String(url).includes('/auth/login') ? rateLimited() : mockFetchResponse({})) as any);

    const { user } = render(<Login />);
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.type(screen.getByLabelText(/password/i), 'password123');
    await user.click(screen.getByRole('button', { name: /sign in/i }));

    expect(await screen.findByText('Rate limit exceeded. Please try again in 37 seconds.')).toBeInTheDocument();
    expect(screen.queryByText('Too many requests')).not.toBeInTheDocument();
  });

  it('shows a plain failure, not a parser error, when the answer is not JSON', async () => {
    vi.mocked(global.fetch).mockImplementation(((url: string) =>
      String(url).includes('/auth/login')
        ? Promise.resolve({
            ok: false,
            status: 502,
            json: () => Promise.reject(new SyntaxError('Unexpected token < in JSON at position 0')),
          })
        : mockFetchResponse({})) as any);

    const { user } = render(<Login />);
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.type(screen.getByLabelText(/password/i), 'password123');
    await user.click(screen.getByRole('button', { name: /sign in/i }));

    expect(await screen.findByText('Login failed')).toBeInTheDocument();
    expect(screen.queryByText(/unexpected token/i)).not.toBeInTheDocument();
  });

  it('says how long to wait when a passkey sign-in is rate limited', async () => {
    Object.defineProperty(window, 'PublicKeyCredential', {
      configurable: true,
      value: class PublicKeyCredentialMock {},
    });
    vi.mocked(global.fetch).mockImplementation(((url: string) =>
      String(url).includes('/passkey/login/start') ? rateLimited() : mockFetchResponse({})) as any);

    const { user } = render(<Login />);
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.click(await screen.findByRole('button', { name: /sign in with passkey/i }));

    expect(await screen.findByText('Rate limit exceeded. Please try again in 37 seconds.')).toBeInTheDocument();
  });

  it('disables button during submission', async () => {
    vi.mocked(global.fetch).mockImplementation(() => 
      new Promise(resolve => setTimeout(() => resolve(mockFetchResponse({ token: mockToken }) as any), 100))
    );

    const { user } = render(<Login />);
    
    await user.type(screen.getByLabelText(/email address/i), 'test@example.com');
    await user.type(screen.getByLabelText(/password/i), 'password123');
    
    const button = screen.getByRole('button', { name: /sign in/i });
    await user.click(button);

    expect(button).toBeDisabled();
  });
});





