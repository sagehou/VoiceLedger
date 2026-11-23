pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_a_version() {
        assert!(!super::version().is_empty());
    }
}
