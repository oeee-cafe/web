import { transformWithEsbuild, type Plugin } from "vite";

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

/**
 * The same for the site's own scripts (vite.site.config.ts), which are built
 * as ES entries because library mode will not make IIFEs of several at once.
 * One that neither imports nor exports is a script, and the page loads two
 * of them as classic scripts; wrapped in a function, their minified names
 * stay out of `window` there and nothing changes for the one loaded as a
 * module. A chunk that shares code or exports something could not be loaded
 * that way at all, so it fails the build rather than the page.
 *
 * Vite also leaves an ES library's whitespace in, for whoever bundles it
 * next. Nobody does here, and htmx alone is 30 kB of it, so it goes once
 * the chunk is wrapped. Only the whitespace: the syntax was lowered and the
 * names shortened already.
 */
export function containScripts(): Plugin {
  return {
    name: "contain-scripts",
    apply: "build",
    enforce: "post",
    async generateBundle(_options, bundle) {
      for (const file of Object.values(bundle)) {
        if (file.type !== "chunk") continue;
        if (!file.isEntry || file.imports.length || file.exports.length) {
          throw new Error(`${file.fileName} is not a standalone script`);
        }
        const wrapped = `(function(){${file.code}\n})();\n`;
        const { code } = await transformWithEsbuild(wrapped, file.fileName, {
          loader: "js",
          minifyWhitespace: true,
          // As vite.site.config.ts says: served with no charset.
          charset: "ascii",
        });
        file.code = code;
      }
    },
  };
}
