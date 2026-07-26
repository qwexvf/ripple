import { AdminClient, UserClient } from "./clients";

export function asAdmin(): string {
  const client = new AdminClient();
  return client.send();
}

export function asUser(): string {
  const client = new UserClient();
  return client.send();
}

// `client` is bound in neither this function nor at module level: another
// function's local must not leak in here
export function unbound(): string {
  return client.send();
}
