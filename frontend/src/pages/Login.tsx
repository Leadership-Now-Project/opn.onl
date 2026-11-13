import { useState, useEffect } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { Loader2, Fingerprint, Mail, Send, CheckCircle } from 'lucide-react';
import { motion } from 'framer-motion';
import { API_ENDPOINTS, authFetch, emailVerificationRequired, responseError } from '../config/api';
import SEO from '../components/SEO';
import logger from '../utils/logger';
import { SIGN_IN_NEEDS_STORAGE, safeLocalStorage } from '../utils/storage';

// Google Identity Services, as far as this page uses it: the OAuth code flow in
// a popup. https://developers.google.com/identity/oauth2/web/guides/use-code-model
const GSI_SCRIPT_URL = 'https://accounts.google.com/gsi/client';

interface GoogleCodeResponse { code?: string; error?: string }
interface GoogleCodeClientConfig {
    client_id: string;
    scope: string;
    ux_mode: 'popup';
    callback: (response: GoogleCodeResponse) => void;
    error_callback?: (error: { type?: string }) => void;
}
declare global {
    interface Window {
        google?: {
            accounts: {
                oauth2: {
                    initCodeClient: (config: GoogleCodeClientConfig) => { requestCode: () => void };
                };
            };
        };
    }
}

const GOOGLE_ACCOUNT_REFUSED =
    'This Google account is not allowed. Use your Leadership Now Project account.';
const GOOGLE_FAILED = 'Google sign-in failed. Please try again.';

let gsiScript: Promise<void> | null = null;

/**
 * Load Google's script once. Called only after /auth/settings named a client
 * id: never from index.html and never while prerendering (no server answers
 * there), so an instance without Google sign-in loads nothing from Google.
 */
function loadGsiScript(): Promise<void> {
    if (window.google?.accounts?.oauth2) return Promise.resolve();
    if (!gsiScript) {
        gsiScript = new Promise<void>((resolve, reject) => {
            const script = document.createElement('script');
            script.src = GSI_SCRIPT_URL;
            script.async = true;
            script.onload = () => resolve();
            script.onerror = () => {
                // Let the next click try again.
                gsiScript = null;
                script.remove();
                reject(new Error('Could not load Google sign-in. Check your connection and try again.'));
            };
            document.head.appendChild(script);
        });
    }
    return gsiScript;
}

/** Open Google's popup and resolve with the one-time authorization code. */
function requestGoogleCode(clientId: string): Promise<string> {
    return new Promise<string>((resolve, reject) => {
        const oauth2 = window.google?.accounts?.oauth2;
        if (!oauth2) {
            reject(new Error(GOOGLE_FAILED));
            return;
        }
        oauth2.initCodeClient({
            client_id: clientId,
            scope: 'openid email profile',
            ux_mode: 'popup',
            callback: (response) => {
                if (response.code) resolve(response.code);
                else reject(new Error(response.error === 'access_denied'
                    ? 'Google sign-in was cancelled.'
                    : GOOGLE_FAILED));
            },
            error_callback: (error) => {
                reject(new Error(
                    error?.type === 'popup_closed' ? 'The Google window was closed before sign-in finished. Please try again.'
                    : error?.type === 'popup_failed_to_open' ? 'Your browser blocked the Google window. Allow pop-ups for this site and try again.'
                    : GOOGLE_FAILED,
                ));
            },
        }).requestCode();
    });
}

function GoogleMark() {
    return (
        <svg className="h-5 w-5" viewBox="0 0 48 48" aria-hidden="true">
            <path fill="#EA4335" d="M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z" />
            <path fill="#4285F4" d="M46.98 24.55c0-1.57-.15-3.09-.38-4.55H24v9.02h12.94c-.58 2.96-2.26 5.48-4.78 7.18l7.73 6c4.51-4.18 7.09-10.36 7.09-17.65z" />
            <path fill="#FBBC05" d="M10.53 28.59c-.48-1.45-.76-2.99-.76-4.59s.27-3.14.76-4.59l-7.98-6.19C.92 16.46 0 20.12 0 24c0 3.88.92 7.54 2.56 10.78l7.97-6.19z" />
            <path fill="#34A853" d="M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.73-6c-2.15 1.45-4.92 2.3-8.16 2.3-6.26 0-11.57-4.22-13.47-9.91l-7.98 6.19C6.51 42.62 14.62 48 24 48z" />
        </svg>
    );
}

export default function Login() {
    const [email, setEmail] = useState('');
    const [password, setPassword] = useState('');
    const [loading, setLoading] = useState(false);
    const [passkeyLoading, setPasskeyLoading] = useState(false);
    const [error, setError] = useState('');
    const [needsVerification, setNeedsVerification] = useState(false);
    const [resendLoading, setResendLoading] = useState(false);
    const [resendSuccess, setResendSuccess] = useState(false);
    const [resendError, setResendError] = useState('');
    // Shown until the server says the instance has passkeys turned off. The
    // page is prerendered with no server to ask, and an instance that has them
    // (the default) should not see the button jump in once /auth/settings answers.
    const [passkeysEnabled, setPasskeysEnabled] = useState(true);
    // Set once /auth/settings says the instance signs people in with Google;
    // the page then offers only that.
    const [googleClientId, setGoogleClientId] = useState<string | null>(null);
    const [googleLoading, setGoogleLoading] = useState(false);
    // False until the /auth/settings request has settled, either way. Until
    // then the page does not know which way of signing in to offer, so it
    // offers none: the password form would otherwise show for a moment on an
    // instance that signs in with Google.
    const [settingsSettled, setSettingsSettled] = useState(false);
    const navigate = useNavigate();

    // Redirect to dashboard if already logged in. Nobody is signed in just by
    // loading this page: after logging out, it waits for a click.
    useEffect(() => {
        const token = safeLocalStorage.getItem('token');
        if (token) {
            navigate('/dashboard', { replace: true });
        }
    }, [navigate]);

    useEffect(() => {
        let cancelled = false;
        (async () => {
            try {
                const res = await fetch(API_ENDPOINTS.appSettings);
                if (!res.ok) return;
                const settings = await res.json();
                if (cancelled) return;
                if (settings.passkeys_enabled === false) setPasskeysEnabled(false);
                if (typeof settings.google_client_id === 'string' && settings.google_client_id) {
                    setGoogleClientId(settings.google_client_id);
                }
            } catch {
                // Offline or an unreadable answer: keep the default. Signing in
                // with a password does not depend on it.
            } finally {
                if (!cancelled) setSettingsSettled(true);
            }
        })();
        return () => { cancelled = true; };
    }, []);

    const handleSubmit = async (e: React.FormEvent) => {
        e.preventDefault();
        setLoading(true);
        setError('');
        setNeedsVerification(false);

        try {
            const res = await fetch(API_ENDPOINTS.login, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ email, password }),
            });

            if (!res.ok) throw new Error(await responseError(res, 'Login failed'));
            const data = await res.json();

            if (!safeLocalStorage.setItem('token', data.token)) throw new Error(SIGN_IN_NEEDS_STORAGE);
            safeLocalStorage.setItem('is_admin', data.is_admin ? 'true' : 'false');

            // The account is unverified. Ask them to verify only when this
            // instance requires it; a settings read that fails still does.
            if (data.email_verified === false && await emailVerificationRequired()) {
                setNeedsVerification(true);
                return;
            }

            navigate('/dashboard');
        } catch (err: unknown) {
            setError(err instanceof Error ? err.message : 'Login failed');
        } finally {
            setLoading(false);
        }
    };

    // Fetch Google's script as soon as the instance says it uses Google, so the
    // click below opens the popup at once (a popup opened after a slow load
    // can be blocked). A failure here is retried, and reported, on click.
    useEffect(() => {
        if (googleClientId) loadGsiScript().catch(() => {});
    }, [googleClientId]);

    const handleGoogleLogin = async () => {
        if (!googleClientId) return;
        setGoogleLoading(true);
        setError('');

        try {
            await loadGsiScript();
            const code = await requestGoogleCode(googleClientId);

            const res = await fetch(API_ENDPOINTS.googleLogin, {
                method: 'POST',
                // The server refuses a code that does not come with this header.
                headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'XmlHttpRequest' },
                body: JSON.stringify({ code }),
            });

            if (!res.ok) {
                const message = await responseError(res, GOOGLE_FAILED);
                // 403 is also a disabled account; that one keeps the server's words.
                throw new Error(res.status === 403 && message !== 'This account is disabled'
                    ? GOOGLE_ACCOUNT_REFUSED
                    : message);
            }
            const data = await res.json();

            if (!safeLocalStorage.setItem('token', data.token)) throw new Error(SIGN_IN_NEEDS_STORAGE);
            safeLocalStorage.setItem('is_admin', data.is_admin ? 'true' : 'false');

            if (data.email_verified === false && await emailVerificationRequired()) {
                setNeedsVerification(true);
                return;
            }

            navigate('/dashboard');
        } catch (err: unknown) {
            setError(err instanceof Error ? err.message : GOOGLE_FAILED);
        } finally {
            setGoogleLoading(false);
        }
    };

    const handleResendVerification = async () => {
        setResendLoading(true);
        setResendSuccess(false);
        setResendError('');

        try {
            // Signed in by now: with the token the server says what really
            // happened (sent, cooling down, no mail on this instance) instead
            // of the anonymous "if the account exists" answer.
            const res = await authFetch(API_ENDPOINTS.resendVerification, {
                method: 'POST',
                body: JSON.stringify({ email }),
            });

            if (res.ok) {
                setResendSuccess(true);
                setTimeout(() => setResendSuccess(false), 5000);
            } else {
                setResendError(await responseError(res, 'Could not resend the verification email'));
            }
        } catch {
            setResendError('Could not resend the verification email');
        } finally {
            setResendLoading(false);
        }
    };

    const handleContinueUnverified = () => {
        navigate('/dashboard');
    };

    const handlePasskeyLogin = async () => {
        if (!email) {
            setError('Please enter your email address first');
            return;
        }

        setPasskeyLoading(true);
        setError('');

        try {
            // Step 1: Start authentication
            const startRes = await fetch(API_ENDPOINTS.passkeyLoginStart, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ username: email }),
            });

            if (!startRes.ok) {
                throw new Error(await responseError(startRes, 'No passkey found for this account'));
            }

            // `login_id` names this attempt; the server answers `finish` only
            // for the id it handed out here.
            const { options, login_id: loginId } = await startRes.json();

            // Step 2: Get credential from browser
            const credential = await navigator.credentials.get({
                publicKey: {
                    ...options.publicKey,
                    challenge: base64ToBuffer(options.publicKey.challenge),
                    allowCredentials: options.publicKey.allowCredentials?.map((c: { id: string; type: string }) => ({
                        ...c,
                        id: base64ToBuffer(c.id),
                    })),
                },
            }) as PublicKeyCredential;

            if (!credential) {
                throw new Error('Authentication cancelled');
            }

            const response = credential.response as AuthenticatorAssertionResponse;

            // Step 3: Finish authentication
            const finishRes = await fetch(API_ENDPOINTS.passkeyLoginFinish, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    login_id: loginId,
                    credential: {
                        id: credential.id,
                        rawId: bufferToBase64(credential.rawId),
                        response: {
                            authenticatorData: bufferToBase64(response.authenticatorData),
                            clientDataJSON: bufferToBase64(response.clientDataJSON),
                            signature: bufferToBase64(response.signature),
                            userHandle: response.userHandle ? bufferToBase64(response.userHandle) : null,
                        },
                        type: credential.type,
                    },
                }),
            });

            if (!finishRes.ok) {
                throw new Error(await responseError(finishRes, 'Authentication failed'));
            }
            const data = await finishRes.json();

            if (!safeLocalStorage.setItem('token', data.token)) throw new Error(SIGN_IN_NEEDS_STORAGE);
            safeLocalStorage.setItem('is_admin', data.is_admin ? 'true' : 'false');

            if (data.email_verified === false && await emailVerificationRequired()) {
                setNeedsVerification(true);
                return;
            }

            navigate('/dashboard');
        } catch (err: unknown) {
            logger.error('Passkey login error:', err);
            setError(err instanceof Error ? err.message : 'Passkey authentication failed');
        } finally {
            setPasskeyLoading(false);
        }
    };

    // Check if WebAuthn is supported
    const isPasskeySupported = typeof window !== 'undefined' && 
        window.PublicKeyCredential !== undefined;

    // Show verification needed screen
    if (needsVerification) {
        return (
            <div className="flex min-h-[80vh] items-center justify-center px-4 py-16">
                <SEO
                    title="Log in"
                    description="Log in to opn.onl to manage your short links, branded QR codes and click analytics."
                    url="/login"
                />
                <motion.div
                    initial={{ opacity: 0, y: 16 }}
                    animate={{ opacity: 1, y: 0 }}
                    transition={{ duration: 0.5, ease: [0.16, 1, 0.3, 1] }}
                    className="w-full max-w-md"
                >
                    <div className="rounded-2xl border border-line bg-surface p-8 shadow-card text-center">
                        <div className="mx-auto mb-5 flex h-14 w-14 items-center justify-center rounded-2xl bg-warning/10">
                            <Mail className="h-7 w-7 text-warning" aria-hidden="true" />
                        </div>
                        <h1 className="font-display text-2xl font-bold text-ink">Verify your email</h1>
                        <p className="mt-2 text-muted leading-relaxed">
                            Check your inbox for a verification link to unlock every feature.
                        </p>
                        <div className="mt-6 space-y-3">
                            <button
                                onClick={handleResendVerification}
                                disabled={resendLoading || resendSuccess}
                                className="flex w-full items-center justify-center gap-2 rounded-xl bg-primary-600 py-3 font-semibold text-white transition-colors hover:bg-primary-700 disabled:opacity-50"
                            >
                                {resendLoading ? <Loader2 className="h-4 w-4 animate-spin" />
                                    : resendSuccess ? <><CheckCircle className="h-4 w-4" /> Email sent</>
                                    : <><Send className="h-4 w-4" /> Resend verification email</>}
                            </button>
                            {resendError && (
                                <p role="alert" className="text-sm text-danger">{resendError}</p>
                            )}
                            <button
                                onClick={handleContinueUnverified}
                                className="w-full rounded-xl border border-line2 bg-surface py-3 font-medium text-ink transition-colors hover:border-ink/30"
                            >
                                Continue to dashboard
                            </button>
                        </div>
                        <p className="mt-4 text-xs text-faint">Some features stay limited until you verify.</p>
                    </div>
                </motion.div>
            </div>
        );
    }

    return (
        <div className="flex min-h-[80vh] items-center justify-center px-4 py-16">
            <SEO
                title="Log in"
                description="Log in to opn.onl to manage your short links, branded QR codes and click analytics."
                url="/login"
            />
            <motion.div
                initial={{ opacity: 0, y: 16 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ duration: 0.5, ease: [0.16, 1, 0.3, 1] }}
                className="w-full max-w-md"
            >
                <div className="mb-8 text-center">
                    <p className="font-mono text-xs uppercase tracking-[0.2em] text-primary-600">Welcome back</p>
                    <h1 className="mt-3 font-display text-3xl font-extrabold tracking-tight text-ink">Sign in to opn.onl</h1>
                    {settingsSettled && !googleClientId && (
                    <p className="mt-2 text-sm text-muted">
                        New here?{' '}
                        <Link to="/register" className="font-medium text-primary-600 underline decoration-primary-300 underline-offset-2 hover:decoration-primary-600 hover:text-primary-700">Create an account</Link>
                    </p>
                    )}
                </div>

                {!settingsSettled ? (
                <div
                    role="status"
                    aria-label="Loading sign-in options"
                    className="flex justify-center rounded-2xl border border-line bg-surface p-8 shadow-card"
                >
                    <Loader2 className="h-6 w-6 animate-spin text-faint" aria-hidden="true" />
                </div>
                ) : googleClientId ? (
                <div className="rounded-2xl border border-line bg-surface p-8 shadow-card">
                    <button
                        type="button"
                        onClick={handleGoogleLogin}
                        disabled={googleLoading}
                        className="flex w-full items-center justify-center gap-3 rounded-xl border border-line2 bg-white px-4 py-3 font-semibold text-ink shadow-subtle transition-colors hover:border-primary-300 hover:bg-primary-50/50 disabled:opacity-70"
                    >
                        {googleLoading
                            ? <Loader2 className="h-5 w-5 animate-spin" aria-hidden="true" />
                            : <GoogleMark />}
                        Sign in with Google
                    </button>
                    {error && (
                        <motion.div
                            initial={{ opacity: 0, y: -8 }} animate={{ opacity: 1, y: 0 }}
                            role="alert" aria-live="polite"
                            className="mt-5 rounded-xl border border-danger/30 bg-danger/5 px-3 py-2.5 text-sm text-danger"
                        >
                            {error}
                        </motion.div>
                    )}
                </div>
                ) : (
                <div className="rounded-2xl border border-line bg-surface p-8 shadow-card">
                    {isPasskeySupported && passkeysEnabled && (
                        <>
                            <button
                                type="button"
                                onClick={handlePasskeyLogin}
                                disabled={passkeyLoading || !email}
                                aria-label="Sign in with passkey"
                                className="flex w-full items-center justify-center gap-3 rounded-xl border border-line2 bg-surface px-4 py-3 font-medium text-ink transition-colors hover:border-primary-300 hover:bg-primary-50/50 disabled:opacity-50 disabled:cursor-not-allowed"
                            >
                                {passkeyLoading
                                    ? <Loader2 className="h-5 w-5 animate-spin" aria-hidden="true" />
                                    : <Fingerprint className="h-5 w-5 text-primary-600" aria-hidden="true" />}
                                Continue with passkey
                            </button>
                            <p className="mt-2 text-center text-xs text-faint">Enter your email first, then use your passkey.</p>

                            <div className="relative my-6">
                                <div className="absolute inset-0 flex items-center"><div className="w-full border-t border-line" /></div>
                                <div className="relative flex justify-center"><span className="bg-surface px-3 font-mono text-xs uppercase tracking-wider text-faint">or email</span></div>
                            </div>
                        </>
                    )}

                    <form method="post" onSubmit={handleSubmit} className="space-y-5">
                        <div>
                            <label htmlFor="email" className="mb-1.5 block text-sm font-medium text-ink">Email address</label>
                            <div className="relative">
                                <Mail className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-faint" aria-hidden="true" />
                                <input
                                    id="email" name="email" type="email" required autoComplete="email"
                                    className="block w-full rounded-xl border border-line2 bg-white py-2.5 pl-10 pr-3 text-ink shadow-subtle outline-none transition-colors focus:border-primary-500"
                                    placeholder="you@example.com"
                                    value={email}
                                    onChange={(e) => setEmail(e.target.value)}
                                />
                            </div>
                        </div>
                        <div>
                            <div className="mb-1.5 flex items-center justify-between">
                                <label htmlFor="password" className="block text-sm font-medium text-ink">Password</label>
                                <Link to="/forgot-password" className="text-sm font-medium text-primary-600 hover:text-primary-700">Forgot?</Link>
                            </div>
                            <input
                                id="password" name="password" type="password" required autoComplete="current-password"
                                className="block w-full rounded-xl border border-line2 bg-white px-3 py-2.5 text-ink shadow-subtle outline-none transition-colors focus:border-primary-500"
                                placeholder="••••••••"
                                value={password}
                                onChange={(e) => setPassword(e.target.value)}
                            />
                        </div>

                        {error && (
                            <motion.div
                                initial={{ opacity: 0, y: -8 }} animate={{ opacity: 1, y: 0 }}
                                role="alert" aria-live="polite"
                                className="rounded-xl border border-danger/30 bg-danger/5 px-3 py-2.5 text-sm text-danger"
                            >
                                {error}
                            </motion.div>
                        )}

                        <button
                            type="submit"
                            disabled={loading}
                            className="flex w-full justify-center rounded-xl bg-primary-600 px-4 py-3 font-semibold text-white transition-colors hover:bg-primary-700 disabled:opacity-70"
                        >
                            {loading ? <Loader2 className="h-5 w-5 animate-spin" aria-hidden="true" /> : 'Sign in'}
                        </button>
                    </form>
                </div>
                )}
            </motion.div>
        </div>
    );
}

// Helper functions for WebAuthn
function base64ToBuffer(base64: string): ArrayBuffer {
    const base64Standard = base64.replace(/-/g, '+').replace(/_/g, '/');
    const padding = '='.repeat((4 - base64Standard.length % 4) % 4);
    const binary = atob(base64Standard + padding);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
        bytes[i] = binary.charCodeAt(i);
    }
    return bytes.buffer;
}

function bufferToBase64(buffer: ArrayBuffer): string {
    const bytes = new Uint8Array(buffer);
    let binary = '';
    for (let i = 0; i < bytes.length; i++) {
        binary += String.fromCharCode(bytes[i]);
    }
    return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
