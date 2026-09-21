use super::*;
use tempfile::tempdir;

#[test]
fn role_query_reads_cached_prose_after_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("client.db");
    let store = onlyne_store::ClientStore::open(&path).unwrap();
    store
        .put_prose("planner", "cluster b exposes planner", "hash-b")
        .unwrap();
    store.put_config("role", "planner").unwrap();
    let machine = IntentMachine::new(store, 3, vec![1]);
    let cli = LocalCli::new(machine);
    let value = cli.export_prose().unwrap();
    assert_eq!(
        value.data.unwrap()["roles"][0]["prose"],
        "cluster b exposes planner"
    );
    drop(cli);
    let reopened = onlyne_store::ClientStore::open(&path).unwrap();
    assert_eq!(
        reopened.prose("planner").unwrap(),
        Some(("cluster b exposes planner".into(), "hash-b".into()))
    );
}
