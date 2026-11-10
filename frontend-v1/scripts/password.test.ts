import assert from 'node:assert/strict';
import { test } from 'node:test';
import { isValidPassword } from '../src/utils/password';

test('password validation agrees with the API byte limits', () => {
  for (const length of [0, 6, 7, 129]) assert.equal(isValidPassword('a'.repeat(length)), false);
  for (const length of [8, 60, 61, 128]) assert.equal(isValidPassword('a'.repeat(length)), true);
  assert.equal(isValidPassword('密'.repeat(2)), false);
  assert.equal(isValidPassword('密'.repeat(3)), true);
  assert.equal(isValidPassword('密'.repeat(43)), false);
});
