use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::{Keypair, PublicKey, Signature};
use ed25519_dalek::{Signer, Verifier};
use sha2::{Digest, Sha256};

use anyhow::{Result, anyhow};

use bytemuck::{Pod, Zeroable};
use serde::{Serialize, Deserialize, Deserializer}; 
use serde_bytes;

use crate::db::VALIDATORS_ADDRESS;
use crate::network::type_of;
use crate::cast;

pub const HASH_BYTE_SIZE: usize = 32;
pub type Sha256Bytes = [u8; HASH_BYTE_SIZE];
pub type Key256Bytes = [u8; 32]; // public/private key

#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone)]
#[repr(transparent)]
pub struct SignatureBytes([u8; Signature::BYTE_SIZE]);

impl Serialize for SignatureBytes { 
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
        where
            S: serde::Serializer {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for SignatureBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where D: Deserializer<'de>
    {
        let bytes: Vec<u8> = serde_bytes::deserialize(deserializer)?;
        let mut array = [0u8; Signature::BYTE_SIZE];
        array.copy_from_slice(&bytes);
        Ok(SignatureBytes(array))
    }
}


pub const TXS_PER_BLOCK: usize = 2;
pub const POW_N_ZEROS: usize = 3; // note: needs to be > 0
pub const POW_LEN_ZEROS: usize = POW_N_ZEROS - 1;

// defines the Account, Transaction, and Block structures of the blockchain

pub trait ChainDigest { 
    // todo: use a proc macro to derive
    fn digest(&self) -> Sha256Bytes;
}

/* ACCOUNT STRUCTS */
// todo: account nonce so txs can only be valid once 
#[derive(BorshDeserialize, BorshSerialize, Default, Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct UserAccount {
    pub address: Key256Bytes,
    pub amount: u128,
}

// this contains all the validators on the network
#[derive(BorshDeserialize, BorshSerialize, Default, Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct ValidatorsAccount {
    pub pubkeys: Vec<Key256Bytes>,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub enum Account { 
    UserAccount(UserAccount), 
    ValidatorsAccount(ValidatorsAccount)
}

impl Default for Account { 
    fn default() -> Self {
        Self::UserAccount(UserAccount::default())
    }
}

impl ChainDigest for Account {
    fn digest(&self) -> Sha256Bytes {
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes); // copy
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

// [(digest, pubkey_bytes)]
#[derive(BorshDeserialize, BorshSerialize, Clone)] 
pub struct AccountDigests(pub Vec<(Sha256Bytes, Key256Bytes)>);

impl ChainDigest for AccountDigests {
    fn digest(&self) -> Sha256Bytes {
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes); // copy
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

/* TRANSACTION STRUCTS */
#[repr(C)]
#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone, Default, Serialize, Deserialize)]
pub struct AccountTransaction {
    pub pubkey: Key256Bytes,
    pub amount: u128,
}

#[repr(C)]
#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone, Default, Serialize, Deserialize)]
pub struct AddValidatorTransaction {
    pub pubkey: Key256Bytes,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub enum Transaction { 
    AccountTransaction(AccountTransaction), 
    AddValidatorTransaction(AddValidatorTransaction)
}

impl Default for Transaction { 
    fn default() -> Self {
        Self::AccountTransaction(AccountTransaction::default())
    }
}

impl ChainDigest for Transaction {
    fn digest(&self) -> Sha256Bytes {
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().as_slice().try_into().unwrap()
    }
}


impl Transaction {
    // consumes
    pub fn sign(self, keypair: &Keypair) -> SignedTransaction {
        let digest = self.digest();
        let sig = keypair.sign(digest.as_slice());
        let sig_bytes = sig.to_bytes();
        SignedTransaction {
            transaction: self,
            signature: SignatureBytes(sig_bytes),
        }
    }

    pub fn verify(&self) -> Result<()> { 
        Ok(())
    }

    pub fn get_sender(&self) -> Result<PublicKey> { 
        let pubkey_bytes = match self { 
            Self::AddValidatorTransaction(tx) => &tx.pubkey, 
            Self::AccountTransaction(tx) => &tx.pubkey
        };
        Ok(PublicKey::from_bytes(pubkey_bytes)?)
    }

    pub fn get_address(&self) -> Result<Key256Bytes> { 
        let bytes = match self { 
            Self::AddValidatorTransaction(_) => VALIDATORS_ADDRESS, 
            Self::AccountTransaction(tx) => tx.pubkey
        };
        Ok(bytes)
    }

    pub fn process(&self, account: Option<Account>) -> Result<Account> {
        //todo 
        match self { 
            Transaction::AccountTransaction(tx) => { 
                if account.is_none() { 
                    // new account
                    Ok(Account::UserAccount(UserAccount { 
                        address: tx.pubkey, 
                        amount: tx.amount
                    }))
                } else { 
                    // update account
                    let account = account.unwrap();
                    let mut account = cast!(account, Account::UserAccount);
                    account.amount = tx.amount; 
                    Ok(Account::UserAccount(account))
                }
            }, 

            Transaction::AddValidatorTransaction(tx) => { 
                let account = account.unwrap();
                let mut account = cast!(account, Account::ValidatorsAccount);

                // update account
                if !account.pubkeys.contains(&tx.pubkey) { 
                    account.pubkeys.push(tx.pubkey);
                }

                Ok(Account::ValidatorsAccount(account))
            }
        }
    }
}

// each Transaction should be applied to a specific kind of account(s)
    // ie, AddValidator => ValidatorsAccount 
    // ie, Transfer => UserAccount

// how to design this into the code? 
    // what do we want? 
        // address = transaction.get_account_address 
        // account = lookup(address, state)
        // account = transaction.process(account)
        // state = store(account, state)


#[repr(C)]
#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct SignedTransaction {
    pub transaction: Transaction,
    pub signature: SignatureBytes,
}

impl Default for SignedTransaction { 
    fn default() -> Self {
        SignedTransaction { 
            transaction: Transaction::default(), 
            signature: SignatureBytes([0; Signature::BYTE_SIZE])
        }
    }
}

impl SignedTransaction {
    pub fn verify(&self) -> anyhow::Result<()> { 
        self.verify_signature()?;
        self.transaction.verify()
    }

    pub fn verify_signature(&self) -> anyhow::Result<()> {
        let digest = self.transaction.digest();
        let publickey = self.transaction.get_sender()?;
        let sig = Signature::from_bytes(self.signature.0.as_slice())?;
        Ok(publickey.verify(digest.as_slice(), &sig)?)
    }
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone, Serialize, Deserialize)]
pub struct Transactions(pub Vec<SignedTransaction>);

impl ChainDigest for Transactions {
    fn digest(&self) -> Sha256Bytes {
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes); // copy
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

impl Default for Transactions { 
    fn default() -> Self {
        Transactions(vec![])
    }
}

/* BLOCK STRUCTS */
// we use serde for rpc 

#[repr(C)]
#[derive(Pod, Zeroable, Copy, Debug, Default, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub parent_hash: Sha256Bytes,
    pub state_root: Sha256Bytes,
    pub tx_root: Sha256Bytes,
    pub block_hash: Sha256Bytes,
    pub nonce: u128,
    pub height: u128,
}

impl ChainDigest for BlockHeader { 
    fn digest(&self) -> Sha256Bytes {
        self.block_hash
    }
}

#[repr(C)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub txs: Transactions,
}

impl Block {
    pub fn genesis() -> Self {
        let header = BlockHeader::genesis();
        let txs = Transactions::default();
        Block { header, txs }
    }
}

impl ChainDigest for Block { 
    fn digest(&self) -> Sha256Bytes {
        self.header.digest()
    }
}

impl BlockHeader {
    pub fn genesis() -> Self {
        let block = BlockHeader {
            state_root: [0; HASH_BYTE_SIZE],
            tx_root: [0; HASH_BYTE_SIZE],
            parent_hash: [0; HASH_BYTE_SIZE],
            block_hash: [0; HASH_BYTE_SIZE],
            nonce: 0,
            height: 0,
        };
        block
    }

    pub fn commit_block_hash(&mut self) {
        self.block_hash = self.compute_block_hash();
    }

    pub fn compute_block_hash(&self) -> Sha256Bytes {
        let mut hasher = Sha256::new();
        hasher.update(self.state_root);
        hasher.update(self.parent_hash);
        hasher.update(self.nonce.to_le_bytes());

        let bytes: Sha256Bytes = hasher.finalize().as_slice().try_into().unwrap();
        bytes
    }

    pub fn is_valid_pow(&self) -> bool {
        let hash = self.compute_block_hash();
        let n_zeros = POW_LEN_ZEROS.min(HASH_BYTE_SIZE);
        let mut pow_success = true;
        for value in hash.iter().take(n_zeros) {
            if *value != 0 {
                pow_success = false;
                break;
            }
        }
        pow_success
    }
}
