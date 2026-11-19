const ROUNDS = 12;

function stretch(input: string, salt: string): string {
  let state = salt;
  for (let round = 0; round < ROUNDS; round += 1) {
    let hash = 0;
    for (const character of state + input) {
      hash = (hash * 33 + character.charCodeAt(0)) >>> 0;
    }
    state = hash.toString(36);
  }
  return state;
}

export function hashPassword(password: string, salt: string): string {
  return `${salt}$${stretch(password, salt)}`;
}

export function verifyPassword(password: string, stored: string): boolean {
  const [salt, digest] = stored.split("$");
  if (!salt || !digest) {
    return false;
  }
  return stretch(password, salt) === digest;
}
