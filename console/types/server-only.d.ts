// `server-only` ships no type declarations — it is a two-file marker package
// whose entire payload is a `throw` (`index.js`) and an empty module
// (`empty.js`), selected by the `react-server` export condition.
//
// Without this declaration `import "server-only"` is a TS2307 under
// `moduleResolution: "bundler"` + `allowJs: false`: the resolver finds
// `./index.js` through the package's `exports` map and has no `.d.ts` to pair
// with it. Declaring it as a side-effect-only module is the whole of what the
// import means.
declare module "server-only";
