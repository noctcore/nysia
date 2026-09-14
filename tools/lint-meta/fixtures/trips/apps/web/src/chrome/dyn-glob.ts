// Form 4 of #19: Vite's own glob import, which need not name the file at all.
//
// This mentions neither `StoreContext` nor `import(`, and it hands back every module in the
// store directory — the provider included.
const modules = import.meta.glob('../store/*.ts');

export const reached = modules;
