export async function loadUser(id: string) {
  return fetch(`/api/v1/users/${id}`);
}

export async function createUser(body: string) {
  return fetch("/api/v1/users", { method: "POST" });
}

export async function unmatched() {
  return fetch("/api/v1/nothing-declares-this");
}

export async function session() {
  return fetch("/auth/session");
}
