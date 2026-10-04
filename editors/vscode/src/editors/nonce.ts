/** A fresh CSP nonce: 128 bits from the platform CSPRNG (Web Crypto; never Math.random), base64. */
export function newNonce(): string {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return btoa(String.fromCharCode(...bytes));
}
