pub fn run() -> u8 {
    let site = Site::new();
    site.step();
    site.stream()
}
