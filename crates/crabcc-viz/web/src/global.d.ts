// Side-effect-only CSS imports — esbuild handles the bundling. Without
// this declaration tsgo flags the import as missing types.
declare module "*.css";

// `bun:test` is provided by the bun runtime; types come from
// `@types/bun` upstream but the workspace currently scopes to
// `@types/react` + `@typescript/native-preview` only. A minimal local
// surface is enough for our test suite — this keeps the dep tree thin.
// Matcher set covers what the tests actually use; extend on demand
// rather than mirroring jest/bun's full matcher list.
declare module "bun:test" {
  interface Matchers<T> {
    toBe: (v: T) => void;
    toEqual: (v: unknown) => void;
    toContain: (v: string) => void;
    toHaveLength: (n: number) => void;
    toBeDefined: () => void;
    toBeUndefined: () => void;
    toBeNull: () => void;
    toBeGreaterThan: (n: number) => void;
    toBeGreaterThanOrEqual: (n: number) => void;
    toBeLessThan: (n: number) => void;
    toBeLessThanOrEqual: (n: number) => void;
    toBeTruthy: () => void;
    toBeFalsy: () => void;
    rejects: { toThrow: (m?: RegExp | string) => Promise<void> };
    not: Matchers<T>;
  }
  export const describe: (name: string, fn: () => void) => void;
  export const it: (name: string, fn: () => void | Promise<void>) => void;
  export const test: (name: string, fn: () => void | Promise<void>) => void;
  export const expect: <T>(actual: T) => Matchers<T>;
  export const beforeEach: (fn: () => void) => void;
  export const afterEach: (fn: () => void) => void;
  export const beforeAll: (fn: () => void) => void;
  export const afterAll: (fn: () => void) => void;
  export const mock: <T>(fn: T) => T;
}
