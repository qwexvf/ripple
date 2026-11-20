pub struct Site;

impl Site {
    pub fn new() -> Self {
        Site
    }

    pub fn step(&self) {
        self.tick();
    }

    fn tick(&self) {}
}

pub trait Stream {
    fn stream(&self) -> u8;
}

impl Stream for Site {
    fn stream(&self) -> u8 {
        1
    }
}
