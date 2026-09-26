/** `?raw` imports resolve to the file's source text (see `webpackFinal` in main.ts). */
declare module "*.css?raw" {
  const source: string;
  export default source;
}
