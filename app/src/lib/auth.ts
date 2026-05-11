let reloading = false;

export function handleAuthFailure() {
  if (reloading) return;
  reloading = true;

  const reload = () => window.location.reload();

  if ('serviceWorker' in navigator) {
    navigator.serviceWorker
      .getRegistrations()
      .then((rs) => Promise.all(rs.map((r) => r.unregister())))
      .finally(reload);
  } else {
    reload();
  }
}

export function isLikelyAuthFailure(error: unknown): boolean {
  return error instanceof TypeError;
}
