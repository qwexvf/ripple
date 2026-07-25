export class Service {
  handle(): number {
    return 1;
  }
}

export class Other {
  handle(): number {
    return 2;
  }
}

function useTyped(s: Service): number {
  return s.handle();
}

function useNew(): number {
  const s = new Service();
  return s.handle();
}

// a one-line method whose body calls the same-named method on another class:
// the Elixir definition-header guard must not mistake this for a definition
export class Inline {
  handle() { const s = new Service(); return s.handle(); }
}
