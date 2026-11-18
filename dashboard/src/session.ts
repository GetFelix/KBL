// What one tab remembers across a reload: the gateway session (from the view link) and
// the person token. Both live in sessionStorage, so they stay in this tab, go with it
// when it closes and are never in the address, localStorage or the store. Storage can
// be missing or throw; the page then works until it is reloaded.

const GATEWAY = 'kbl.session.gateway';
const PERSON = 'kbl.session.person';

export interface GatewaySession {
  gateway: string | null;
  room: string;
  token: string;
}

const read = (key: string): string | null => {
  try {
    return sessionStorage.getItem(key);
  } catch {
    return null;
  }
};
const write = (key: string, value: string | null) => {
  try {
    if (value === null) sessionStorage.removeItem(key);
    else sessionStorage.setItem(key, value);
  } catch {
    // Not remembered past this page.
  }
};

export function saveGateway(s: GatewaySession) {
  write(GATEWAY, JSON.stringify(s));
}

export function loadGateway(): GatewaySession | null {
  try {
    const v = JSON.parse(read(GATEWAY) ?? 'null');
    return v && typeof v.room === 'string' && typeof v.token === 'string' && (v.gateway === null || typeof v.gateway === 'string') ? v : null;
  } catch {
    return null;
  }
}

export function forgetGateway() {
  write(GATEWAY, null);
}

export const savePerson = (token: string) => write(PERSON, token);
export const loadPerson = (): string | null => read(PERSON);
export const forgetPerson = () => write(PERSON, null);
