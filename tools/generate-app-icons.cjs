// Regenerate from frontend/pwa/logo.svg and the app's default theme palette:
// npm install --prefix /tmp/openwebide-icon-tools --no-audit --no-fund @resvg/resvg-js@2.6.2
// NODE_PATH=/tmp/openwebide-icon-tools/node_modules node tools/generate-app-icons.cjs
const { readFileSync, writeFileSync } = require('node:fs');
const { resolve } = require('node:path');
const { Resvg } = require('@resvg/resvg-js');

const root = resolve(__dirname, '..');
const output = resolve(root, 'frontend/pwa');
const logo = readFileSync(resolve(output, 'logo.svg'), 'utf8');
const styles = readFileSync(resolve(root, 'frontend/styles.css'), 'utf8');
const palette = styles.match(/:root\s*\{([^}]+)\}/)[1];
const token = name => palette.match(new RegExp(`--${name}:\\s*(#[0-9a-f]+);`, 'i'))[1];
const artwork = logo.match(/<path\b[^>]+\/>/)[0];

function icon(scale, rounded) {
  const inset = 16 * (1 - scale);
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <rect width="32" height="32" rx="${rounded ? 6 : 0}" fill="${token('bg')}"/>
  <g fill="${token('accent')}" transform="translate(${inset} ${inset}) scale(${scale})">${artwork}</g>
</svg>\n`;
}

// Desktop icons use the canvas; adaptive icons retain their safe-circle padding.
for (const size of [192, 512]) {
  for (const [prefix, svg] of [['icon', icon(0.94, true)], ['maskable-icon', icon(0.64, false)]]) {
    const png = new Resvg(svg, { fitTo: { mode: 'width', value: size } }).render().asPng();
    writeFileSync(resolve(output, `${prefix}-${size}.png`), png);
  }
}
writeFileSync(resolve(output, 'apple-touch-icon.png'), new Resvg(icon(0.94, false), { fitTo: { mode: 'width', value: 180 } }).render().asPng());
const favicon = icon(0.875, true);
writeFileSync(resolve(output, 'favicon.svg'), favicon);
writeFileSync(resolve(output, 'favicon.png'), new Resvg(favicon, { fitTo: { mode: 'width', value: 32 } }).render().asPng());
