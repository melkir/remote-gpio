const RECOVERY_KEY = 'somfy.access.recovery.startedAt';
const RECOVERY_COOLDOWN_MS = 60_000;

export async function handleAccessSessionExpiry() {
  const now = Date.now();
  const previous = Number(window.sessionStorage.getItem(RECOVERY_KEY) ?? '0');
  if (Number.isFinite(previous) && now - previous < RECOVERY_COOLDOWN_MS) {
    return;
  }
  window.sessionStorage.setItem(RECOVERY_KEY, String(now));

  if ('serviceWorker' in navigator) {
    await navigator.serviceWorker
      .getRegistrations()
      .then((registrations) =>
        Promise.all(registrations.map((registration) => registration.unregister())),
      )
      .catch((error: unknown) => {
        console.warn('[Access] Failed to unregister service workers:', error);
      });
  }

  window.location.assign(window.location.href);
}

export function isAccessChallenge(response: Response): boolean {
  if (response.status === 401 || response.status === 403 || response.redirected) {
    return true;
  }

  const contentType = response.headers.get('content-type') ?? '';
  return contentType.includes('text/html');
}

export function isValidChannel(value: string): boolean {
  return ['L1', 'L2', 'L3', 'L4', 'ALL'].includes(value);
}
