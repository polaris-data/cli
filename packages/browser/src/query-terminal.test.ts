import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';

const require = createRequire(import.meta.url);
const inkPictureEntry = require.resolve('ink-picture');
const queryTerminalModule = pathToFileURL(
  path.join(path.dirname(inkPictureEntry), 'utils/queryTerminal.js'),
).href;

const { decodeInputChunk } = (await import(queryTerminalModule)) as {
  decodeInputChunk(data: unknown): string;
};

test('decodes terminal responses delivered as Uint8Array bytes', () => {
  const response = Uint8Array.from([27, 91, 63, 49, 59, 50, 99]);

  assert.equal(decodeInputChunk(response), '\u001B[?1;2c');
});

test('preserves Buffer and string input', () => {
  const response = '\u001B[?1;2c';

  assert.equal(decodeInputChunk(Buffer.from(response)), response);
  assert.equal(decodeInputChunk(response), response);
});
