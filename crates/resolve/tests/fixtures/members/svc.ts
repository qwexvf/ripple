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
