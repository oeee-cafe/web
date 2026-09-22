import type { Plugin } from "vite";

/**
 * Keeps a library-mode IIFE to the one global it is named for.
 *
 * Lowering class fields for Firefox 56 makes esbuild emit helpers --
 * `__defProp`, `__publicField` -- and Vite puts them *before* the IIFE, at
 * the top of the file, where a classic script makes each one a property of
 * `window`. Minified, they are called `b` or `Vt`. The replay viewer runs on
 * pages with scripts of their own, and a page that said `var b = ...` at
 * top level replaced `__publicField`, and every replay on it failed with
 * "b is not a function" when the player was constructed.
 *
 * Wrapping the finished file in one more function scope puts the helpers
 * inside it; the library's own name is handed back out as the global it was.
 */
export function containIife(name: string): Plugin {
  return {
    name: "contain-iife",
    apply: "build",
    enforce: "post",
    generateBundle(_options, bundle) {
      for (const file of Object.values(bundle)) {
        if (file.type !== "chunk") continue;
        file.code = `var ${name}=(function(){${file.code}\nreturn ${name};})();\n`;
      }
    },
  };
}
