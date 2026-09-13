// The carve-out: the store module may reach its own context however it likes. If rule (d)
// stopped honouring the allowlist, this file would start reporting and the clean fixture
// would fail — which is what keeps the carve-out from quietly becoming a ban.
const context = await import('./StoreContext');
export const c = context;
