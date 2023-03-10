use std::error::Error;
use rocksdb::DB;
use sha2::{Sha256, Digest};
use borsh::{BorshSerialize, BorshDeserialize};
use ed25519_dalek::{Keypair, Signature, PublicKey};
use ed25519_dalek::{Signer, Verifier};
use rand::rngs::OsRng;

const HASH_BYTE_SIZE: usize = 32;
type Sha256Bytes = [u8; HASH_BYTE_SIZE];
type Key256Bytes = [u8; 32]; // public/private key
type SignatureBytes = [u8; Signature::BYTE_SIZE];

#[derive(BorshDeserialize, BorshSerialize, Default, Debug, PartialEq, Eq, Clone)]
pub struct Account { 
    pub address: Key256Bytes, 
    pub amount: u128
}

#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone)]
struct Transaction { 
    pub address: Key256Bytes, 
    pub amount: u128, 
}

#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone)]
struct SignedTransaction { 
    pub transaction: Transaction,
    pub signature: Option<SignatureBytes>
}

impl Transaction { 
    fn digest(&self) -> Sha256Bytes { 
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().as_slice().try_into().unwrap()
    }

    // consumes 
    fn sign(self, keypair: &Keypair) -> SignedTransaction { 
        let digest = self.digest();
        let sig = keypair.sign(digest.as_slice());
        let sig_bytes = sig.to_bytes();
        SignedTransaction { transaction: self, signature: sig_bytes.try_into().unwrap() }
    }
}

impl SignedTransaction { 
    fn verify(&self) -> bool { 
        let digest = self.transaction.digest();
        // todo: remove these unwrap()s and return a result<>
        let publickey = PublicKey::from_bytes(self.transaction.address.as_slice()).unwrap();
        let sig = Signature::from_bytes(self.signature.unwrap().as_slice()).unwrap();
        publickey.verify(digest.as_slice(), &sig).is_ok()
    }
}

impl Account { 
    fn digest(&self) -> Sha256Bytes { 
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

#[derive(BorshDeserialize, BorshSerialize)]
struct Block { 
    state_root: Sha256Bytes, 
    parent_hash: Sha256Bytes,
    block_hash: Option<Sha256Bytes>,
}

impl Block { 
    pub fn genesis() -> Self { 
        let mut block = Block { state_root: [0; HASH_BYTE_SIZE], parent_hash: [0; HASH_BYTE_SIZE],  block_hash: None };
        block.commit_block_hash();
        block
    }

    pub fn commit_block_hash(&mut self) { 
        let mut hasher = Sha256::new();
        hasher.update(self.state_root);
        hasher.update(self.parent_hash);

        let bytes: Sha256Bytes = hasher.finalize().as_slice().try_into().unwrap();
        self.block_hash = Some(bytes);
    }
}

// [(digest, pubkey_bytes)]
#[derive(BorshDeserialize, BorshSerialize)]
struct AccountDigests(Vec<(Sha256Bytes, Key256Bytes)>);

impl AccountDigests { 
    pub fn compute_root_digest(&self) -> Sha256Bytes { 
        let mut hasher = Sha256::new();
        self.0
            .iter()
            .for_each(|(d, _)| sha2::Digest::update(&mut hasher, d));
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

pub fn main() { 

}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use ed25519_dalek::{Signer, Verifier};
    use sha2::digest::Update;

    #[test]
    fn block_state() { 
        // defn: 
        // state = AccountDigests 
        // account = Account 
        // db = DB 

        // db for tests
        let mut db = HashMap::new();

        // --- 
        // init chain
        let genesis = Block::genesis(); 
        db.insert(genesis.block_hash.unwrap(), genesis.try_to_vec().unwrap());

        let parent_block = genesis; // todo: fork choice struct

        // --- 
        // init new account
        let keypair = example_keypair();
        let account = Account { address: keypair.public.to_bytes(), amount: 10 };

        // add account to db
        let digest = account.digest();
        db.insert(digest, account.try_to_vec().unwrap());

        // compute new state
        let mut account_digests = vec![];
        account_digests.push((digest.clone(), account.address));
        let account_digests = AccountDigests(account_digests);

        // add state to db
        let state_root = account_digests.compute_root_digest();
        db.insert(state_root, account_digests.try_to_vec().unwrap());

        // compute new block 
        let mut block = Block { state_root, parent_hash: parent_block.block_hash.unwrap(), block_hash: None }; 
        block.commit_block_hash();

        // store block in db
        db.insert(block.block_hash.unwrap(), block.try_to_vec().unwrap());

    }

    fn example_keypair() -> Keypair { 
        let mut rng = OsRng{};
        let keypair = Keypair::generate(&mut rng);
        keypair
    }

    #[test]
    fn transaction_test() { 
        let keypair = example_keypair();
        let publickey = keypair.public;

        let transaction = Transaction { 
            address: publickey.to_bytes(), 
            amount: 1, 
        };
        let transaction = transaction.sign(&keypair);

        assert!(transaction.verify())
    }

    #[test]
    fn keypair_test() { 
        let mut rng = OsRng{};
        let keypair = Keypair::generate(&mut rng);

        let msg = b"hello";
        let signature = keypair.sign(msg);
        let _bytes = signature.to_bytes();

        let publickey = keypair.public; 
        let result = publickey.verify(msg, &signature);
        assert!(result.is_ok());

        let _account = Account { 
            address: publickey.to_bytes(), 
            amount: 0
        };
        let _publickey = PublicKey::from_bytes(&_account.address).unwrap();
        assert_eq!(_publickey, publickey);
    }
    
    #[test] 
    fn db_insert() { 
        let path = "../db/";
        let db = DB::open_default(path).unwrap();

        let account = Account::default(); 
        let value = account.try_to_vec().unwrap();
        let key = account.digest();

        db.put(key.clone(), value).unwrap();

        match db.get(key) { 
            Ok(Some(v)) => {
                let _account = Account::try_from_slice(v.as_slice()).unwrap();
                assert_eq!(_account, account);
            }, 
            _ => { assert!(false) }
        }
    }

    #[test] 
    fn account_serialization() { 
        let hasher = Sha256::new();
        let result = hasher.chain("some address").finalize().to_vec();
        let result = &result[..32];
        
        let account = Account {
            address: result.try_into().unwrap(),
            amount: 1000,
        };

        let bytes = account.try_to_vec().unwrap();
        let _account = Account::try_from_slice(bytes.as_slice()).unwrap();

        assert_eq!(_account, account);
    }

    #[test] 
    fn hash_test() { 
       let mut hasher = Sha256::new();
       sha2::Digest::update(&mut hasher, b"hi");
       let result = hasher.finalize();
       let result = result.to_vec();
       println!("{result:?}");
    }

}