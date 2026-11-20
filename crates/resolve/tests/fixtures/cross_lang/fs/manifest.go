package fs

import "os"

func readDeps(path string) {
	os.ReadFile(path)
}
