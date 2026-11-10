export const PASSWORD_MIN_BYTES = 8;
export const PASSWORD_MAX_BYTES = 128;

export function isValidPassword(value: string): boolean {
  const length = new TextEncoder().encode(value).length;
  return length >= PASSWORD_MIN_BYTES && length <= PASSWORD_MAX_BYTES;
}
