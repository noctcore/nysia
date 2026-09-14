// Form 3 of #19: a line whose trim starts with a block-comment opener.
//
// The guard skipped any line starting with `/*` wholesale, so everything after the comment
// closed again was invisible — the comment did not even have to be about the import.
/* lazily, to keep the entry chunk small */ const store = await import('../store/StoreContext');

export const reached = store;
