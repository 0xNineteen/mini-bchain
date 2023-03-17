use bytemuck::{Pod};
use rocksdb::{DB, DBPinnableSlice};
use borsh::{BorshDeserialize, BorshSerialize};
use anyhow::{Result, anyhow};


use crate::structures::*;

pub trait ChainDB { 
    fn get<T: Pod>(&self, key: Sha256Bytes) -> Result<T>;
    fn get_pinned(&self, key: Sha256Bytes) -> Result<DBPinnableSlice>;
    fn get_vec<T: BorshDeserialize>(&self, key: Sha256Bytes) -> Result<T>;
    fn put<T: Pod + ChainDigest>(&self, value: &T) -> Result<Sha256Bytes>;
    fn put_vec<T: BorshSerialize + ChainDigest>(&self, value: &T) -> Result<Sha256Bytes>;
}

// for prod
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

impl ChainDB for RocksDB { 
    fn get<T: Pod>(&self, key: Sha256Bytes) -> Result<T> {
        Ok(*bytemuck::try_from_bytes(
            self.db.get(key)?.unwrap().as_slice()
        ).map_err(|o| anyhow!("db get casting error: {o:?}"))?)
    }

    fn get_vec<T: BorshDeserialize>(&self, key: Sha256Bytes) -> Result<T> {
        // copy :(
        Ok(T::try_from_slice(
            self.db.get(key)?.unwrap().as_slice()
        )?)
    }

    // should use macro get_pinned!
    fn get_pinned(&self, key: Sha256Bytes) -> Result<DBPinnableSlice> {
        let pinned_data = self.db.get_pinned(key)?.unwrap();
        // cant do deserialization here :(
        Ok(pinned_data)
    }

    fn put<T: Pod + ChainDigest>(&self, value: &T) -> Result<Sha256Bytes> {
        let key = value.digest();
        self.db.put(key, bytemuck::bytes_of(value))?;
        Ok(key)
    }

    fn put_vec<T: BorshSerialize + ChainDigest>(&self, value: &T) -> Result<Sha256Bytes> {
        let key = value.digest();
        self.db.put(key, value.try_to_vec()?)?;
        Ok(key)
    }
}


#[cfg(test)] 
mod tests { 
    use super::*;
    use std::ops::Deref;

    #[test]
    pub fn db_tests() { 
        let path = "./db";
        let db = DB::open_default(path).unwrap();
        let db = RocksDB { db };

        // these tests require only a single thread on the db so we do it like this

        account_insert_test(db).unwrap(); 
    }

    pub fn account_insert_test(db: RocksDB) -> Result<()> { 
        let account = Account::default(); 
        let key = db.put(&account)?;

        let _account = db.get(key)?;
        assert_eq!(account, _account);

        get_pinned!(db key => pinned_account);
        assert_eq!(account, *pinned_account);

        // cleanup 
        db.db.delete(key)?;

        Ok(())
    }


}