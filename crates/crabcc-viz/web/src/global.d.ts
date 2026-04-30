// Side-effect-only CSS imports — esbuild handles the bundling. Without
// this declaration tsgo flags the import as missing types.
declare module "*.css";

// `bun:test` is provided by the bun runtime; types come from
// `@types/bun` upstream but the workspace currently scopes to
// `@types/react` + `@typescript/native-preview` only. A minimal local
// surface is enough for our test suite — this keeps the dep tree thin.
declare module "bun:test" {
  export const describe: (name: string, fn: () => void) => void;
  export const it: (name: string, fn: () => void | Promise<void>) => void;
  export const expect: <T>(actual: T) => {
    toBe: (v: T) => void;
    toEqual: (v: unknown) => void;
    toContain: (v: string) => void;
    toHaveLength: (n: number) => void;
    rejects: { toThrow: (m?: RegExp | string) => Promise<void> };
  };
  export const beforeEach: (fn: () => void) => void;
  export const afterEach: (fn: () => void) => void;
  export const mock: <T>(fn: T) => T;
}
