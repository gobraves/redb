#![no_main]

use std::collections::BTreeMap;
use libfuzzer_sys::fuzz_target;
use tempfile::NamedTempFile;
use redb::{Database, Durability, Error, ReadableTable, TableDefinition, WriteStrategy};

mod common;
use common::*;

const TABLE_DEF: TableDefinition<u64, [u8]> = TableDefinition::new("fuzz_table");

fuzz_target!(|config: FuzzConfig| {
    let redb_file: NamedTempFile = NamedTempFile::new().unwrap();
    let write_strategy = if config.use_checksums {
        WriteStrategy::CommitLatency
    } else {
        WriteStrategy::Throughput
    };
    let db = unsafe { Database::builder().set_write_strategy(write_strategy).create(redb_file.path(), config.max_db_size.value) };

    if matches!(db, Err(Error::OutOfSpace)) {
        return;
    }
    let db = db.unwrap();

    let mut reference = BTreeMap::new();

    for transaction in config.transactions.iter() {
        let reference_backup = reference.clone();
        let mut txn = db.begin_write().unwrap();
        // We're not trying to test crash safety, so don't bother with durability
        if !transaction.durable {
            txn.set_durability(Durability::None);
        }
        {
            let mut table = txn.open_table(TABLE_DEF).unwrap();
            for op in transaction.ops.iter() {
                match op {
                    FuzzOperation::Get { key } => {
                        let key = key.value;
                        match reference.get(&key) {
                            Some(reference_len) => {
                                let value = table.get(&key).unwrap().unwrap();
                                assert_eq!(value.len(), *reference_len);
                            },
                            None => {
                                assert!(table.get(&key).unwrap().is_none());
                            }
                        }
                    },
                    FuzzOperation::Insert { key, value_size } => {
                        let key = key.value;
                        let value_size = value_size.value as usize;
                        let value = vec![0xFF; value_size];
                        if let Err(err) = table.insert(&key, &value) {
                            assert!(matches!(err, Error::OutOfSpace) && config.oom_plausible());
                            return;
                        }
                        reference.insert(key, value_size);
                    },
                    FuzzOperation::Remove { key } => {
                        let key = key.value;
                        match reference.remove(&key) {
                            Some(reference_len) => {
                                match table.remove(&key) {
                                    Ok(value) => {
                                        assert_eq!(value.unwrap().to_value().len(), reference_len);
                                    },
                                    Err(err) => {
                                        assert!(matches!(err, Error::OutOfSpace) && config.oom_plausible());
                                        return;
                                    }
                                }
                            },
                            None => {
                                assert!(table.remove(&key).unwrap().is_none());
                            }
                        }
                    },
                    FuzzOperation::Len {} => {
                        assert_eq!(reference.len(), table.len().unwrap());
                    }
                    FuzzOperation::Range { start_key, len, reversed } => {
                        let start = start_key.value;
                        let end = start + len.value;
                        let mut reference_iter: Box<dyn Iterator<Item=(&u64, &usize)>> = if *reversed {
                            Box::new(reference.range(start..end).rev())
                        } else {
                            Box::new(reference.range(start..end))
                        };
                        let mut iter = if *reversed {
                            table.range(start..end).unwrap().rev()
                        } else {
                            table.range(start..end).unwrap()
                        };
                        while let Some((ref_key, ref_value_len)) = reference_iter.next() {
                            let (key, value) = iter.next().unwrap();
                            assert_eq!(*ref_key, key);
                            assert_eq!(*ref_value_len, value.len());
                        }
                        assert!(iter.next().is_none());
                    },
                }
            }
        }
        if transaction.commit {
            if let Err(err) = txn.commit() {
                assert!(matches!(err, Error::OutOfSpace) && config.oom_plausible());
                return;
            }
        } else {
            txn.abort().unwrap();
            reference = reference_backup;
        }
    }
});
