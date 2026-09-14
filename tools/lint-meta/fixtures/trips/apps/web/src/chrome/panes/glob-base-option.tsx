// `base` moves the directory a relative pattern resolves against, so a pattern that reaches
// nothing beside this file reaches the store instead.
//
// This file is `.tsx` and the pattern is `*.ts` on purpose: without that the pattern would
// match this file itself and the rule would report it for the wrong reason, which is a proof
// that passes whether or not the defect is there.
const modules = import.meta.glob('./*.ts', { base: '../../store' });

export const reached = modules;
