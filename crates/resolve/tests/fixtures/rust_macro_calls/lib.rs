pub struct Parser;

impl Parser {
    pub fn new() -> Self {
        Parser
    }

    pub fn ok(&self) -> bool {
        true
    }
}

pub fn parse(x: u8) -> u8 {
    x
}

pub fn check() {
    let p = Parser::new();
    assert!(self_check(&p));
}

fn self_check(p: &Parser) -> bool {
    p.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse(1), 1);
        assert!(Parser::new().ok());
    }
}
