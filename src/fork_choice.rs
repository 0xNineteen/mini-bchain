use std::collections::HashMap;
use std::collections::BinaryHeap;
use anyhow::anyhow;
use anyhow::Result;
use solana_metrics::datapoint_info;
use tracing::info;

use crate::structures::*;

pub struct ForkChoice {
    block_heights: HashMap<Sha256Bytes, u32>,
    // sorted so we know it will be consistent across nodes
    heads: BinaryHeap<Sha256Bytes>,
    head_height: u32,
}

impl ForkChoice {
    pub fn new(block_hash: Sha256Bytes) -> Self {
        let mut block_heights = HashMap::new();
        let mut heads = BinaryHeap::new();
        let head_height = 0;

        block_heights.insert(block_hash, 0);
        heads.push(block_hash);

        ForkChoice {
            block_heights,
            heads,
            head_height,
        }
    }

    pub fn insert(&mut self, block_hash: Sha256Bytes, parent_hash: Sha256Bytes) -> Result<()> {
        let parent_height = self.block_heights.get(&parent_hash);
        if parent_height.is_none() {
            return Err(anyhow!("parent block hash DNE in fork choice"));
        }
        let parent_height = parent_height.unwrap();
        let block_height = parent_height + 1;

        if block_height > self.head_height {
            // new head
            self.heads.clear();
            self.head_height = block_height;
            self.heads.push(block_hash);

            info!("new head length: {block_height}!");
            datapoint_info!(
                "chain", 
                ("height", block_height, i64), 
            );
        } else if block_height == self.head_height {
            // another tie
            self.heads.push(block_hash);
        }
        self.block_heights.insert(block_hash, block_height);

        Ok(())
    }

    pub fn get_head(&self) -> Option<Sha256Bytes> {
        self.heads.peek().cloned()
    }

    pub fn exists(&self, block_hash: Sha256Bytes) -> bool { 
        self.block_heights.get(&block_hash).map(|_| true).unwrap_or(false)
    }
}


#[cfg(test)]
mod tests { 
    use super::*; 

    #[test]
    pub fn test_genesis() { 
        // a
        // head = a
        let genesis = BlockHeader::genesis(); 
        let genesis_hash = genesis.compute_block_hash();

        let fc = ForkChoice::new(genesis_hash); 

        let head = fc.get_head().unwrap();
        assert_eq!(head, genesis_hash);
    }

    #[test]
    pub fn test_chain() { 
        // a -> b 
        // head = b
        let genesis = BlockHeader::genesis(); 
        let genesis_hash = genesis.compute_block_hash();

        let mut fc = ForkChoice::new(genesis_hash); 

        let mut bh = BlockHeader::default(); 
        bh.parent_hash = genesis_hash;
        let block_hash = bh.compute_block_hash();

        fc.insert(block_hash, bh.parent_hash).unwrap();
        
        let head = fc.get_head().unwrap();
        assert_eq!(head, block_hash);
    }

    #[test]
    pub fn test_fork() { 
        // a -> b -> c 
        //   -> d
        // head = c 

        // a 
        let genesis = BlockHeader::genesis(); 
        let genesis_hash = genesis.compute_block_hash();
        let mut fc = ForkChoice::new(genesis_hash); 

        // b 
        let mut bh = BlockHeader::default(); 
        bh.parent_hash = genesis_hash;
        let block_hash = bh.compute_block_hash();
        fc.insert(block_hash, bh.parent_hash).unwrap();

        // c
        let mut bh = BlockHeader::default(); 
        bh.parent_hash = block_hash;
        let c_block_hash = bh.compute_block_hash();
        fc.insert(c_block_hash, bh.parent_hash).unwrap();
        
        // d
        let mut bh = BlockHeader::default(); 
        bh.parent_hash = genesis_hash;
        bh.nonce = 300; // different nonce = different hash 
        fc.insert(bh.compute_block_hash(), bh.parent_hash).unwrap();

        let head = fc.get_head().unwrap();
        assert_eq!(head, c_block_hash);
    }

    #[test]
    fn test_parent_not_found() { 
        let genesis = BlockHeader::genesis(); 
        let genesis_hash = genesis.compute_block_hash();
        let mut fc = ForkChoice::new(genesis_hash); 

        let bh = BlockHeader::default(); 
        // note: parent hash not set = ZEROs
        let block_hash = bh.compute_block_hash();
        let result = fc.insert(block_hash, bh.parent_hash);

        assert!(result.is_err());
    }

}