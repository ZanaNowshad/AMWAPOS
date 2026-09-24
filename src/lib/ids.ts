// Operation ids for exactly-once backend operations (ULID-like: time-sortable, 26 chars).
const ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

export function newOperationId(): string {
  let t = Date.now();
  let time = "";
  for (let i = 0; i < 10; i++) {
    time = ALPHABET[t % 32] + time;
    t = Math.floor(t / 32);
  }
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let rand = "";
  for (let i = 0; i < 16; i++) rand += ALPHABET[bytes[i] % 32];
  return time + rand;
}
