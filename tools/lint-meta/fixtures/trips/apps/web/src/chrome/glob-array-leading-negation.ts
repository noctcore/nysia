// The same hole reached through a negation rather than an extension. The first entry
// excludes one stylesheet, so the first-literal check read `.css` and exempted the call
// before it ever reached the pattern that does the work.
const modules = import.meta.glob(['!../store/ignored.css', '../store/*.ts']);

export const reached = modules;
