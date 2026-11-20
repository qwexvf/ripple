package usecase

func Snapshot(dir string) error {
	if err := AnalyzeUsage(dir); err != nil {
		return err
	}
	return helper(dir)
}
