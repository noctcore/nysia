// The same hole with an innocent pattern in front of it and options that make the modules
// arrive eagerly — the shape a component would actually write, and every gate green.
const modules = import.meta.glob(['../**/*.css', '**/StoreContext.ts'], {
  eager: true,
  import: 'StoreContext',
});

export const reached = modules;
