// `caseSensitive: false` changes which files the pattern matches, and the rule's matcher
// knows nothing about it — so a pattern that reaches nothing here reaches the store there.
const modules = import.meta.glob('../STORE/*.TS', { caseSensitive: false });

export const reached = modules;
