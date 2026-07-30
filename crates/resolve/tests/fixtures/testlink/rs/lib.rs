pub fn real(x: u32) -> u32 {
    x + 1
}

#[cfg(test)]
mod tests {
    use super::real;

    #[test]
    fn covers_real() {
        // called outside the assert on purpose: a call inside a macro token tree
        // isn't captured, and this fixture is about test scopes, not macros
        let got = real(1);
        assert_eq!(got, 2);
    }
}
