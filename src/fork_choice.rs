use std::collections::HashMap;
use std::collections::BinaryHeap;
use anyhow::anyhow;
use anyhow::Result;
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
}
