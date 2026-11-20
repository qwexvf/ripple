package fs

type HookFS struct{}

func (h HookFS) ReadFile(p string) {}
