use super::*;

#[test]
fn available_reads_injected_env() {
    let (backend, _) = backend(Script::default());
    assert!(backend.available().unwrap());
    let missing = HerdrBackend::with_env(Arc::new(Script::default()), BTreeMap::new());
    assert!(!missing.available().unwrap());
}
