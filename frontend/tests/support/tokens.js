/**
 * The token files, in cascade order: the kit's, then PokeDumpster's theme overrides.
 * Only these two may hold a raw colour literal or declare a token.
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const src = join(here, '../../src/lib');

export const kitTokensPath = join(src, 'kit/tokens.css');
export const themePath = join(src, 'styles/tokens.css');
export const tokenFiles = [kitTokensPath, themePath];
export const contrastPairsPath = join(src, 'kit/contrast-pairs.json');

/** Both files concatenated in cascade order, so last-write-wins is the real cascade. */
export const tokensCss = tokenFiles.map((f) => readFileSync(f, 'utf8')).join('\n');
