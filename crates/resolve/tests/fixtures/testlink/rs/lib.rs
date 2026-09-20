pub fn real(x: u32) -> u32 {
    x + 1
}

#[cfg(test)]
mod tests {
    use super::real;

    // a helper the tests are built from. It exercises nothing, so no Tests edge
    // ever names it — the case `is_test` exists for (#123).
    fn fixture(n: u32) -> u32 {
        n
    }

    #[test]
    fn covers_real() {
        // called outside the assert on purpose: a call inside a macro token tree
        // isn't captured, and this fixture is about test scopes, not macros
        let got = real(fixture(1));
        assert_eq!(got, 2);
    }
}
