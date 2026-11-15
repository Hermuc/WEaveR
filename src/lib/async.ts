/**
 * Resolve `fallback` if `promise` has not settled within `ms`.
 *
 * Used for UI actions whose backend call can legitimately hang (a held
 * webview lock, a stalled Steam request). Without a ceiling the button
 * spins forever and the only escape is restarting the app.
 */
export function withTimeout<T>(
  promise: Promise<T>,
  ms: number,
  fallback: T,
): Promise<T> {
  return Promise.race([
    promise,
    new Promise<T>((resolve) => setTimeout(() => resolve(fallback), ms)),
  ]);
}
