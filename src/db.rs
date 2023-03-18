use std::{fs, path::PathBuf};

use bytemuck::{Pod};
use rocksdb::{DB, DBPinnableSlice};
use borsh::{BorshDeserialize, BorshSerialize};
use anyhow::{Result, anyhow};


use crate::{structures::*, fork_choice::ForkChoice};

pub struct RocksDB { 
    pub db: DB
}

#[macro_export]
macro_rules! get_pinned {
    ($db:ident $hash:ident => $name:ident) => {
        let $name = $db.get_pinned($hash)?; // cant deserialize in fcn so we use macro
        let $name = bytemuck::from_bytes($name.deref()); // zero copy
    };
}

impl RocksDB { 
    pub fn insert_genesis(&self) -> Result<ForkChoice> { 
        // init genesis + database
        let mut genesis = Block::genesis();
        let genesis_hash = genesis.header.block_hash;
        let fork_choice = ForkChoice::new(genesis_hash);

        let account_digests = AccountDigests(vec![]);
        let state_root = account_digests.digest();
        genesis.header.state_root = state_root;
        
        // optimize for batch put
        self.put_vec(&account_digests)?;
        self.put(&genesis)?;

        Ok(fork_choice)
    }

    pub fn get<T: Pod>(&self, key: Sha256Bytes) -> Result<T> {
        let result = self.db.get(key)?.ok_or(anyhow!("db get error: not found"))?; 
        Ok(*bytemuck::try_from_bytes(
            result.as_slice()
        ).map_err(|o| anyhow!("db deserialization error: {o:?}"))?)
    }

    pub fn get_vec<T: BorshDeserialize>(&self, key: Sha256Bytes) -> Result<T> {
        let result = self.db.get(key)?.ok_or(anyhow!("db get error: not found"))?; 
        Ok(T::try_from_slice(result.as_slice())?)
    }

    // should use macro get_pinned!
    pub fn get_pinned(&self, key: Sha256Bytes) -> Result<DBPinnableSlice> {
        let pinned_data = self.db.get_pinned(key)?.unwrap();
        // cant do deserialization here :(
        Ok(pinned_data)
    }

    pub fn put<T: Pod + ChainDigest>(&self, value: &T) -> Result<Sha256Bytes> {
        let key = value.digest();
        self.db.put(key, bytemuck::bytes_of(value))?;
        Ok(key)
    }

    pub fn put_vec<T: BorshSerialize + ChainDigest>(&self, value: &T) -> Result<Sha256Bytes> {
        let key = value.digest();
        self.db.put(key, value.try_to_vec()?)?;
        Ok(key)
    }
}

// solana/ledger/blockstore
// used for db tests
use tempfile::{Builder, TempDir};

#[macro_export]
macro_rules! tmp_ledger_name {
    () => {
        &format!("{}-{}", file!(), line!())
    };
}

#[macro_export]
macro_rules! get_tmp_ledger_path_auto_delete {
    () => {
        $crate::db::get_ledger_path_from_name_auto_delete($crate::tmp_ledger_name!())
    };
}

pub fn get_ledger_path_from_name_auto_delete(name: &str) -> TempDir {
    let mut path = get_ledger_path_from_name(name);
    // path is a directory so .file_name() returns the last component of the path
    let last = path.file_name().unwrap().to_str().unwrap().to_string();
    path.pop();
    fs::create_dir_all(&path).unwrap();
    Builder::new()
        .prefix(&last)
        .rand_bytes(0)
        .tempdir_in(path)
        .unwrap()
}

pub fn get_ledger_path_from_name(name: &str) -> PathBuf {
    let path = [
        "target".to_string(),
        name.to_string(),
    ]
    .iter()
    .collect();

    // whack any possible collision
    let _ignored = fs::remove_dir_all(&path);

    path
}


#[cfg(test)] 
mod tests { 
    use super::*;
    use std::ops::Deref;

    #[test]
    pub fn account_insert_test() -> Result<()> { 
        let path = get_tmp_ledger_path_auto_delete!();
        let db = DB::open_default(path).unwrap();
        let db = RocksDB { db };

        let account = Account::default(); 
        let key = db.put(&account)?;

        let _account = db.get(key)?;
        assert_eq!(account, _account);

        get_pinned!(db key => pinned_account);
        assert_eq!(account, *pinned_account);

        Ok(())
    }


}