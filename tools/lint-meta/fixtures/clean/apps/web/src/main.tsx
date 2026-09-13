// The other carve-out. main.tsx composes the provider, which is the one line wave 2
// changes when the mock store becomes the daemon-backed one.
const context = await import('./store/StoreContext');
export const c = context;
